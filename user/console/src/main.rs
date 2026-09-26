//! The console server: owns the serial ports and the display, and gives
//! other programs a console over IPC (see `libpios::console` for the
//! protocol).
//!
//! `init` starts it with its endpoint and then hands it the hardware the
//! kernel handed over: each UART's registers, and its interrupt if it has
//! one (else it is polled on a timer), and the framebuffer with the text the
//! kernel left on it, so the screen carries on where the kernel stopped.
//!
//! Output goes to every UART and the display. Input from any UART, and
//! from input drivers such as the USB keyboard's (`INPUT` requests), goes
//! into one buffer, which `READ` requests take from; a reader waits (its reply is
//! simply kept) until there is input. While the buffer is full, a UART's
//! interrupt stays masked and its bytes wait in its FIFO, so nothing is
//! lost. The UARTs are also emptied while drawing, which is slow, so their
//! FIFOs don't overflow meanwhile.

#![no_std]
#![no_main]

use libpios::console::{self, MAX_BYTES, OK, REFUSED};
use libpios::{Handle, Message, Reply, TakeOnce, println};
use pios_pl011::Pl011;
use pios_textconsole::{Surface, TextConsole};

libpios::pios_main!(main);

/// Where the hardware is mapped.
const UART_ADDR: [usize; 2] = [0x21_0000_0000, 0x21_0001_0000];
const DISPLAY_ADDR: usize = 0x20_0000_0000;
const TEXT_ADDR: usize = 0x22_0000_0000;

/// Notification badges: bits no client or set-up badge has.
const INTERRUPT_BADGE: [u64; 2] = [1 << 62, 1 << 61];
const POLL_BADGE: u64 = 1 << 63;
const NOTIFICATIONS: u64 = INTERRUPT_BADGE[0] | INTERRUPT_BADGE[1] | POLL_BADGE;

/// How often to poll UARTs without an interrupt.
const POLL_MS: u64 = 10;

const INPUT_SIZE: usize = 4096;
const MAX_READERS: usize = 16;

struct Uart {
    port: Pl011,
    interrupt: Option<Handle>,
    /// The kernel masked its interrupt when it arrived; it stays masked
    /// until the FIFO has been emptied.
    masked: bool,
}

/// Input waiting to be read.
struct Input {
    bytes: [u8; INPUT_SIZE],
    start: usize,
    len: usize,
}

impl Input {
    fn is_full(&self) -> bool {
        self.len == INPUT_SIZE
    }

    fn push(&mut self, byte: u8) {
        debug_assert!(!self.is_full());
        self.bytes[(self.start + self.len) % INPUT_SIZE] = byte;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.bytes[self.start];
        self.start = (self.start + 1) % INPUT_SIZE;
        self.len -= 1;
        Some(byte)
    }
}

/// Move bytes from `port`'s FIFO into `input` while there is room. Returns
/// true if the FIFO was emptied.
fn drain(port: &Pl011, input: &mut Input) -> bool {
    port.clear_rx_interrupt();
    while !input.is_full() {
        match port.try_getc() {
            Some(byte) => input.push(byte),
            None => return true,
        }
    }
    false
}

/// Readers waiting for input, oldest first, with how many bytes each wants.
struct Readers {
    waiting: [Option<(Reply, usize)>; MAX_READERS],
    start: usize,
    len: usize,
}

struct Server {
    uarts: [Option<Uart>; 2],
    display: &'static mut TextConsole,
    input: &'static mut Input,
    readers: Readers,
}

static DISPLAY: TakeOnce<TextConsole> = TakeOnce::new(TextConsole::INACTIVE);
static INPUT: TakeOnce<Input> = TakeOnce::new(Input {
    bytes: [0; INPUT_SIZE],
    start: 0,
    len: 0,
});

/// The display's geometry, from `SETUP_DISPLAY`.
#[derive(Clone, Copy)]
struct Geometry {
    width: usize,
    height: usize,
    pitch: usize,
}

impl Server {
    /// Handle one set-up message from `init`. Returns whether set-up is
    /// done, or `Err` if the message made no sense.
    fn setup(&mut self, message: Message, geometry: &mut Option<Geometry>) -> Result<bool, ()> {
        let data = message.data;
        let which = data[0] as usize;
        match message.label {
            console::SETUP_UART if which < 2 => {
                let memory = message.handle.ok_or(())?;
                memory.map(UART_ADDR[which]).map_err(|_| ())?;
                // SAFETY: init gave us a UART's registers, now mapped there
                // for good.
                let port = unsafe { Pl011::new(UART_ADDR[which]) };
                self.uarts[which] = Some(Uart {
                    port,
                    interrupt: None,
                    masked: false,
                });
            }
            console::SETUP_UART_INTERRUPT if which < 2 => {
                let uart = self.uarts[which].as_mut().ok_or(())?;
                uart.interrupt = Some(message.handle.ok_or(())?);
            }
            console::SETUP_DISPLAY => {
                let [width, height, pitch, size] =
                    [data[0], data[1], data[2], data[3]].map(|d| d as usize);
                let fits = width <= pios_textconsole::MAX_WIDTH
                    && height <= pios_textconsole::MAX_HEIGHT
                    && pitch >= width * 4
                    && size >= pitch * height;
                if !fits {
                    return Err(());
                }
                message
                    .handle
                    .ok_or(())?
                    .map(DISPLAY_ADDR)
                    .map_err(|_| ())?;
                *geometry = Some(Geometry {
                    width,
                    height,
                    pitch,
                });
            }
            console::SETUP_DISPLAY_TEXT => {
                let g = geometry.ok_or(())?;
                let [size, x, y] = [data[0], data[3], data[4]].map(|d| d as usize);
                message.handle.ok_or(())?.map(TEXT_ADDR).map_err(|_| ())?;
                // SAFETY: the text is now mapped read-only at TEXT_ADDR for
                // good. That it is `size` bytes long we take from init, as
                // we take the hardware itself: the kernel made the memory
                // (whole pages) to hold that much, and init passes both on.
                let text = unsafe { core::slice::from_raw_parts(TEXT_ADDR as *const u8, size) };
                if !self.display.resume(surface(g), text, x, y) {
                    return Err(());
                }
            }
            console::SETUP_DONE => {
                // A display without the kernel's text: start afresh.
                if let (Some(g), false) = (*geometry, self.display.active()) {
                    self.display.start(surface(g));
                }
                return Ok(true);
            }
            _ => return Err(()),
        }
        Ok(false)
    }

    /// Collect input: from UARTs whose interrupt arrived (unmasking it once
    /// the FIFO is empty) and from polled ones.
    fn collect_input(&mut self) {
        for uart in self.uarts.iter_mut().flatten() {
            match &uart.interrupt {
                Some(interrupt) if uart.masked => {
                    if drain(&uart.port, self.input) {
                        uart.masked = false;
                        let _ = interrupt.acknowledge_interrupt();
                    }
                }
                Some(_) => {}
                None => {
                    drain(&uart.port, self.input);
                }
            }
        }
    }

    /// Answer waiting readers while there is input.
    fn serve_readers(&mut self) {
        while self.input.len > 0 && self.readers.len > 0 {
            let slot = self.readers.start;
            self.readers.start = (slot + 1) % MAX_READERS;
            self.readers.len -= 1;
            let (reply, wanted) = self.readers.waiting[slot].take().expect("queued");
            let mut bytes = [0u8; MAX_BYTES];
            let mut count = 0;
            while count < wanted {
                match self.input.pop() {
                    Some(byte) => {
                        bytes[count] = byte;
                        count += 1;
                    }
                    None => break,
                }
            }
            let _ = reply.reply(console::pack(OK, &bytes[..count]));
        }
        // There may be room for input left waiting now.
        self.collect_input();
    }

    fn read(&mut self, reply: Reply, wanted: usize) {
        let wanted = wanted.clamp(1, MAX_BYTES);
        if self.readers.len == MAX_READERS {
            let _ = reply.reply(Message::new(REFUSED, &[]));
            return;
        }
        let slot = (self.readers.start + self.readers.len) % MAX_READERS;
        self.readers.waiting[slot] = Some((reply, wanted));
        self.readers.len += 1;
        self.serve_readers();
    }

    fn write(&mut self, bytes: &[u8]) {
        for (i, &byte) in bytes.iter().enumerate() {
            for uart in self.uarts.iter().flatten() {
                if byte == b'\n' {
                    uart.port.putc(b'\r');
                }
                uart.port.putc(byte);
            }
            self.display.putc(byte);
            // Drawing is slow (scrolling redraws much of the screen), so
            // keep the UARTs' FIFOs from overflowing meanwhile.
            if i % 8 == 7 {
                for uart in self.uarts.iter().flatten() {
                    drain(&uart.port, self.input);
                }
            }
        }
    }
}

/// The surface for the mapped framebuffer.
fn surface(g: Geometry) -> Surface {
    // SAFETY: SETUP_DISPLAY mapped `pitch * height` bytes of framebuffer at
    // DISPLAY_ADDR for good, and checked the geometry.
    unsafe { Surface::new(DISPLAY_ADDR as *mut u8, g.width, g.height, g.pitch) }
}

fn main() -> i32 {
    let Some(endpoint) = libpios::start_handle() else {
        println!("console: started without an endpoint");
        return 1;
    };
    let mut server = Server {
        uarts: [None, None],
        display: DISPLAY.take().expect("first use"),
        input: INPUT.take().expect("first use"),
        readers: Readers {
            waiting: [const { None }; MAX_READERS],
            start: 0,
            len: 0,
        },
    };

    // Set-up, from init only.
    let mut geometry = None;
    loop {
        let Ok(request) = endpoint.receive() else {
            return 1;
        };
        let result = if request.badge == console::SETUP_BADGE {
            server.setup(request.message, &mut geometry)
        } else {
            Err(())
        };
        if let Some(reply) = request.reply {
            let label = if result.is_ok() { OK } else { REFUSED };
            let _ = reply.reply(Message::new(label, &[]));
        }
        if result == Ok(true) {
            break;
        }
    }

    let mut polled = false;
    for (i, uart) in server.uarts.iter().enumerate() {
        let Some(uart) = uart else { continue };
        match &uart.interrupt {
            Some(interrupt) => {
                if interrupt
                    .bind_interrupt(&endpoint, INTERRUPT_BADGE[i])
                    .is_err()
                {
                    return 1;
                }
                uart.port.enable_rx_interrupt();
            }
            None => polled = true,
        }
    }
    let _poll_timer = if polled {
        match libpios::timer(&endpoint, POLL_BADGE, POLL_MS) {
            Ok(timer) => Some(timer),
            Err(_) => return 1,
        }
    } else {
        None
    };
    // Anything typed before now is already waiting.
    for uart in server.uarts.iter_mut().flatten() {
        uart.masked = uart.interrupt.is_some();
    }
    server.collect_input();

    loop {
        let Ok(request) = endpoint.receive() else {
            return 1;
        };
        if request.badge & NOTIFICATIONS != 0 {
            for (i, uart) in server.uarts.iter_mut().enumerate() {
                if let Some(uart) = uart
                    && request.badge & INTERRUPT_BADGE[i] != 0
                {
                    uart.masked = true;
                }
            }
            server.collect_input();
            server.serve_readers();
            continue;
        }
        let Some(reply) = request.reply else { continue };
        let message = request.message;
        match (request.badge, message.label) {
            (console::CLIENT_BADGE, console::WRITE) => {
                let mut bytes = [0u8; MAX_BYTES];
                let count = console::unpack(&message, &mut bytes);
                server.write(&bytes[..count]);
                let _ = reply.reply(Message::new(OK, &[]));
            }
            (console::CLIENT_BADGE, console::READ) => server.read(reply, message.data[0] as usize),
            (console::INPUT_BADGE, console::INPUT) => {
                let mut bytes = [0u8; MAX_BYTES];
                let count = console::unpack(&message, &mut bytes);
                // (If the buffer is full, typing is lost, as with a
                // keyboard buffer: there is no FIFO to leave it in.)
                for &byte in &bytes[..count] {
                    if !server.input.is_full() {
                        server.input.push(byte);
                    }
                }
                let _ = reply.reply(Message::new(OK, &[]));
                server.serve_readers();
            }
            _ => {
                let _ = reply.reply(Message::new(REFUSED, &[]));
            }
        }
    }
}
