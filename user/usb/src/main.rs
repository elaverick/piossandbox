//! A USB host controller driver, for keyboards. `init` starts one for each
//! controller, and hands it the controller's registers, a DMA handle, and
//! console handles for what is typed and for messages (see `libpios::usb`).
//!
//! It resets the controller, then looks at the root ports twice a second:
//! a keyboard plugged in is set up (see `keyboard`), and what is typed on it
//! goes to the console server as input, like what arrives on a serial
//! port. Other devices, and hubs, are reported but not used yet.
//!
//! Everything is polled, every 10 ms: the controllers' interrupts are PCIe
//! MSIs on both boards, which pios doesn't set up yet.

#![no_std]
#![no_main]

mod device;
mod dma;
mod keyboard;
mod xhci;

use libpios::console::{self, SETUP_BADGE};
use libpios::{Handle, Message, println, usb};

use device::{Device, DeviceMemory};
use dma::DmaPool;
use keyboard::{Keyboard, Typed};
use xhci::{PORT_CONNECTED, PORT_STATUS_CHANGE, TRANSFER_EVENT, Xhci, now_ms};

libpios::pios_main!(main);

/// Where things are mapped.
const REGISTERS_ADDR: usize = 0x20_0000_0000;
const DMA_ADDR: usize = 0x30_0000_0000;
/// DMA memory: the controller's rings and tables, and 6 pages per port.
const DMA_PAGES: usize = 160;

const TICK_BADGE: u64 = 1;
const TICK_MS: u64 = 10;
const PORT_SCAN_MS: u64 = 500;

/// The most root ports we look after.
const MAX_PORTS: usize = 16;

struct Setup {
    controller: Option<(Handle, u64, usize, u64)>,
    dma: Option<Handle>,
    input: Option<Handle>,
    console: Option<Handle>,
}

impl Setup {
    fn handle(&mut self, message: Message) -> Result<bool, ()> {
        let handle = message.handle;
        match message.label {
            usb::SETUP_CONTROLLER => {
                let [kind, size, number] = [message.data[0], message.data[1], message.data[2]];
                self.controller = Some((handle.ok_or(())?, kind, size as usize, number));
            }
            usb::SETUP_DMA => self.dma = Some(handle.ok_or(())?),
            usb::SETUP_INPUT => self.input = Some(handle.ok_or(())?),
            usb::SETUP_CONSOLE => self.console = Some(handle.ok_or(())?),
            usb::SETUP_DONE => return Ok(true),
            _ => return Err(()),
        }
        Ok(false)
    }
}

/// What is plugged into a root port.
enum Port {
    Empty,
    Keyboard(Keyboard),
    /// Something we don't drive, or couldn't set up.
    Other(Option<Device>),
}

fn main() -> i32 {
    let Some(endpoint) = libpios::start_handle() else {
        return 1;
    };
    let mut setup = Setup {
        controller: None,
        dma: None,
        input: None,
        console: None,
    };
    loop {
        let Ok(request) = endpoint.receive() else {
            return 1;
        };
        let result = if request.badge == SETUP_BADGE {
            setup.handle(request.message)
        } else {
            Err(())
        };
        if let Some(reply) = request.reply {
            let label = if result.is_ok() {
                console::OK
            } else {
                console::REFUSED
            };
            let _ = reply.reply(Message::new(label, &[]));
        }
        if result == Ok(true) {
            break;
        }
    }
    let Setup {
        controller: Some((registers, kind, size, number)),
        dma: Some(dma),
        input: Some(input),
        console: Some(messages),
    } = setup
    else {
        return 1;
    };
    console::connect(messages);

    if size < 0x1000 || registers.map(REGISTERS_ADDR).is_err() {
        println!("usb{}: couldn't map the controller", number);
        return 1;
    }
    let pool = match DmaPool::new(&dma, DMA_ADDR, DMA_PAGES) {
        Ok(pool) => pool,
        Err(e) => {
            println!("usb{}: no DMA memory ({:?})", number, e);
            return 1;
        }
    };
    // SAFETY: init gave us the controller's registers, now mapped there for
    // good, and said whether it is a DWC3.
    let started = unsafe { Xhci::start(REGISTERS_ADDR, kind == pios_abi::USB_DWC3, pool) };
    let mut xhci = match started {
        Ok(xhci) => xhci,
        Err(e) => {
            println!("usb{}: {}", number, e);
            return 1;
        }
    };
    let ports = (xhci.ports as usize).min(MAX_PORTS);
    let mut memory: [Option<DeviceMemory>; MAX_PORTS] = [const { None }; MAX_PORTS];
    let mut state: [Port; MAX_PORTS] = [const { Port::Empty }; MAX_PORTS];

    let Ok(_tick) = libpios::timer(&endpoint, TICK_BADGE, TICK_MS) else {
        return 1;
    };
    let mut next_scan = 0;
    loop {
        let now = now_ms();
        let mut typed = Typed {
            bytes: [0; 64],
            len: 0,
        };
        while let Some(event) = xhci.next_event() {
            match event.kind() {
                TRANSFER_EVENT => {
                    for port in state.iter_mut() {
                        if let Port::Keyboard(keyboard) = port
                            && keyboard.owns(&event)
                            && !keyboard.report(&mut xhci, &event, now, &mut typed)
                        {
                            println!(
                                "usb{}: port {}: the keyboard stopped answering",
                                number, keyboard.device.port
                            );
                        }
                    }
                }
                PORT_STATUS_CHANGE => next_scan = now,
                _ => {}
            }
        }
        for port in state.iter_mut() {
            if let Port::Keyboard(keyboard) = port {
                keyboard.tick(now, &mut typed);
            }
        }
        if typed.len > 0 {
            let _ = console::send_input(&input, &typed.bytes[..typed.len]);
        }

        if now >= next_scan {
            next_scan = now + PORT_SCAN_MS;
            for index in 0..ports {
                let port = index as u8 + 1;
                let connected = xhci.port_status(port) & PORT_CONNECTED != 0;
                match (&state[index], connected) {
                    (Port::Empty, true) => {
                        if memory[index].is_none() {
                            memory[index] = DeviceMemory::new(&mut xhci).ok();
                        }
                        let Some(memory) = &memory[index] else {
                            println!("usb{}: port {}: out of DMA memory", number, port);
                            state[index] = Port::Other(None);
                            continue;
                        };
                        state[index] = attach(&mut xhci, memory, port, number);
                    }
                    (Port::Empty, false) => {}
                    (_, true) => {}
                    (_, false) => {
                        let old = core::mem::replace(&mut state[index], Port::Empty);
                        let device = match old {
                            Port::Keyboard(keyboard) => Some(keyboard.device),
                            Port::Other(device) => device,
                            Port::Empty => None,
                        };
                        if let Some(device) = device {
                            xhci.disable_slot(device.slot);
                        }
                        println!("usb{}: port {}: unplugged", number, port);
                    }
                }
                xhci.clear_port_changes(port);
            }
        }
        // Sleep until the next tick.
        if endpoint.receive().is_err() {
            return 1;
        }
    }
}

/// Set up what was just plugged into `port`.
fn attach(xhci: &mut Xhci, memory: &DeviceMemory, port: u8, number: u64) -> Port {
    let speed = match xhci.enable_port(port) {
        Ok(speed) => speed,
        Err(e) => {
            println!("usb{}: port {}: {}", number, port, e);
            return Port::Other(None);
        }
    };
    let device = match Device::address(xhci, memory, port, speed) {
        Ok(device) => device,
        Err(e) => {
            println!("usb{}: port {}: {}", number, port, e);
            return Port::Other(None);
        }
    };
    match Keyboard::start(xhci, memory, device) {
        Ok(keyboard) => {
            println!("usb{}: port {}: keyboard", number, port);
            Port::Keyboard(keyboard)
        }
        Err((device, what)) => {
            println!("usb{}: port {}: {}", number, port, what);
            Port::Other(Some(device))
        }
    }
}
