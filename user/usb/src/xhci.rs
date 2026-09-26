//! An xHCI host controller: its registers, command and event rings, and
//! the commands and control transfers that set up a device (xHCI 1.2
//! specification, sections 4 and 6).
//!
//! Everything is polled: the controller's interrupts reach the GIC as PCIe
//! MSIs on both boards, which pios doesn't set up yet.

use crate::dma::{DmaPool, PAGE_SIZE, Page, read_barrier, write_barrier};

/// A transfer request block: the unit of every ring.
#[derive(Clone, Copy, Debug, Default)]
pub struct Trb {
    pub param: u64,
    pub status: u32,
    pub control: u32,
}

impl Trb {
    pub fn kind(&self) -> u32 {
        (self.control >> 10) & 0x3F
    }

    /// For events: the completion code.
    pub fn code(&self) -> u32 {
        self.status >> 24
    }

    /// For events: the slot.
    pub fn slot(&self) -> u8 {
        (self.control >> 24) as u8
    }

    /// For transfer events: the endpoint (device context index).
    pub fn endpoint(&self) -> u8 {
        ((self.control >> 16) & 0x1F) as u8
    }
}

// TRB types.
pub const NORMAL: u32 = 1;
const SETUP: u32 = 2;
const DATA: u32 = 3;
const STATUS: u32 = 4;
const LINK: u32 = 6;
const ENABLE_SLOT: u32 = 9;
const DISABLE_SLOT: u32 = 10;
const ADDRESS_DEVICE: u32 = 11;
const CONFIGURE_ENDPOINT: u32 = 12;
const EVALUATE_CONTEXT: u32 = 13;
pub const TRANSFER_EVENT: u32 = 32;
const COMMAND_COMPLETION: u32 = 33;
pub const PORT_STATUS_CHANGE: u32 = 34;

// TRB control bits.
const CYCLE: u32 = 1 << 0;
const TOGGLE_CYCLE: u32 = 1 << 1;
pub const ISP: u32 = 1 << 2;
pub const IOC: u32 = 1 << 5;
const IDT: u32 = 1 << 6;
const DIR_IN: u32 = 1 << 16;

// Completion codes.
pub const SUCCESS: u32 = 1;
pub const SHORT_PACKET: u32 = 13;

/// TRBs per ring page; the last is a link back to the start.
const RING_TRBS: usize = PAGE_SIZE / 16;

/// A producer ring (commands, or one endpoint's transfers).
pub struct Ring {
    page: Page,
    index: usize,
    cycle: bool,
}

impl Ring {
    pub fn new(page: Page) -> Ring {
        page.zero();
        Ring {
            page,
            index: 0,
            cycle: true,
        }
    }

    /// Where the controller should start: the ring's address with its
    /// cycle state.
    pub fn dequeue_pointer(&self) -> u64 {
        self.page.bus | self.cycle as u64
    }

    fn write(&self, index: usize, trb: Trb) {
        let at = index * 16;
        self.page.write64(at, trb.param);
        self.page.write32(at + 8, trb.status);
        // The cycle bit hands the TRB to the controller, so it goes last.
        write_barrier();
        self.page.write32(at + 12, trb.control | self.cycle as u32);
    }

    /// Add `trb` (its cycle bit is set here); returns its bus address.
    pub fn push(&mut self, trb: Trb) -> u64 {
        let bus = self.page.bus + (self.index * 16) as u64;
        self.write(self.index, trb);
        self.index += 1;
        if self.index == RING_TRBS - 1 {
            let link = Trb {
                param: self.page.bus,
                status: 0,
                control: LINK << 10 | TOGGLE_CYCLE,
            };
            self.write(self.index, link);
            self.index = 0;
            self.cycle = !self.cycle;
        }
        bus
    }
}

/// The event ring the controller fills (one segment).
struct EventRing {
    page: Page,
    index: usize,
    cycle: bool,
}

/// Why something failed.
pub type Result<T> = core::result::Result<T, &'static str>;

// Capability registers.
const CAPLENGTH: usize = 0x00;
const HCSPARAMS1: usize = 0x04;
const HCSPARAMS2: usize = 0x08;
const HCCPARAMS1: usize = 0x10;
const DBOFF: usize = 0x14;
const RTSOFF: usize = 0x18;

// Operational registers.
const USBCMD: usize = 0x00;
const USBSTS: usize = 0x04;
const CRCR: usize = 0x18;
const DCBAAP: usize = 0x30;
const CONFIG: usize = 0x38;
const PORTSC: usize = 0x400;

const CMD_RUN: u32 = 1 << 0;
const CMD_RESET: u32 = 1 << 1;
const STS_HALTED: u32 = 1 << 0;
const STS_NOT_READY: u32 = 1 << 11;

// Interrupter 0, in the runtime registers.
const IMAN: usize = 0x20;
const ERSTSZ: usize = 0x28;
const ERSTBA: usize = 0x30;
const ERDP: usize = 0x38;
const ERDP_BUSY: u64 = 1 << 3;

// PORTSC bits.
pub const PORT_CONNECTED: u32 = 1 << 0;
pub const PORT_ENABLED: u32 = 1 << 1;
const PORT_RESET: u32 = 1 << 4;
const PORT_RESET_CHANGE: u32 = 1 << 21;
/// Bits to write back unchanged: read-only, and read-write ones whose
/// write has an effect (writing 1 to the enable or change bits clears
/// them). Linux calls writing these "port state to neutral".
const PORT_KEEP: u32 = (1 << 0)
    | (1 << 3)
    | (0xF << 10)
    | (1 << 30)
    | (0xF << 5)
    | (1 << 9)
    | (0x3 << 14)
    | (0x7 << 25);
const PORT_CHANGES: u32 = 0x7F << 17;

// DWC3 global registers (the Pi 5's controllers), after the xHCI ones.
const DWC3_GCTL: usize = 0xC110;
const DWC3_GCTL_PRTCAPDIR: u32 = 0x3 << 12;
const DWC3_GCTL_HOST: u32 = 0x1 << 12;

/// Device speeds, as PORTSC and the slot context number them.
pub const FULL_SPEED: u8 = 1;
pub const LOW_SPEED: u8 = 2;
pub const HIGH_SPEED: u8 = 3;

/// The most slots we use.
const MAX_SLOTS: u32 = 16;

/// Transfer events that arrived while waiting for something else, for the
/// main loop.
const STASH: usize = 16;

pub struct Xhci {
    op: usize,
    runtime: usize,
    doorbells: usize,
    pub ports: u8,
    /// 32 or 64 bytes.
    pub context_size: usize,
    dcbaa: Page,
    commands: Ring,
    events: EventRing,
    pub pool: DmaPool,
    stash: [Option<Trb>; STASH],
}

fn read(addr: usize) -> u32 {
    // SAFETY: callers only pass controller registers, mapped (as device
    // memory) by `Xhci::new`'s caller.
    unsafe { (addr as *const u32).read_volatile() }
}

fn write(addr: usize, value: u32) {
    // SAFETY: as for `read`.
    unsafe { (addr as *mut u32).write_volatile(value) }
}

fn write64(addr: usize, value: u64) {
    write(addr, value as u32);
    write(addr + 4, (value >> 32) as u32);
}

/// Milliseconds since start-up (for timeouts).
pub fn now_ms() -> u64 {
    libpios::counter() / (libpios::counter_frequency() / 1000).max(1)
}

/// Wait up to `ms` milliseconds for `done`.
fn wait_for(ms: u64, mut done: impl FnMut() -> bool) -> bool {
    let deadline = now_ms() + ms;
    loop {
        if done() {
            return true;
        }
        if now_ms() > deadline {
            return false;
        }
        libpios::yield_now();
    }
}

impl Xhci {
    /// Reset the controller whose registers are mapped at `base` and start
    /// it, with its rings in memory from `pool`.
    ///
    /// # Safety
    ///
    /// An xHCI controller's registers (and, if `dwc3`, a DWC3's global
    /// registers after them) must be mapped at `base` for good.
    pub unsafe fn start(base: usize, dwc3: bool, mut pool: DmaPool) -> Result<Xhci> {
        if dwc3 {
            // The DWC3 can be a device or a host: make it a host.
            let gctl = read(base + DWC3_GCTL);
            write(
                base + DWC3_GCTL,
                (gctl & !DWC3_GCTL_PRTCAPDIR) | DWC3_GCTL_HOST,
            );
        }
        let op = base + (read(base + CAPLENGTH) & 0xFF) as usize;
        let hcs1 = read(base + HCSPARAMS1);
        let hcs2 = read(base + HCSPARAMS2);
        let hcc1 = read(base + HCCPARAMS1);
        let mut pages = || pool.page().ok_or("not enough DMA memory");
        let dcbaa = pages()?;
        let commands = Ring::new(pages()?);
        let event_page = pages()?;
        let erst = pages()?;
        let mut xhci = Xhci {
            op,
            runtime: base + (read(base + RTSOFF) & !0x1F) as usize,
            doorbells: base + (read(base + DBOFF) & !0x3) as usize,
            ports: (hcs1 >> 24) as u8,
            context_size: if hcc1 & (1 << 2) != 0 { 64 } else { 32 },
            dcbaa,
            commands,
            events: EventRing {
                page: event_page,
                index: 0,
                cycle: true,
            },
            pool,
            stash: [None; STASH],
        };

        // Stop and reset it.
        if !wait_for(1000, || read(op + USBSTS) & STS_NOT_READY == 0) {
            return Err("the controller never became ready");
        }
        write(op + USBCMD, read(op + USBCMD) & !CMD_RUN);
        if !wait_for(100, || read(op + USBSTS) & STS_HALTED != 0) {
            return Err("the controller didn't stop");
        }
        write(op + USBCMD, CMD_RESET);
        if !wait_for(1000, || {
            read(op + USBCMD) & CMD_RESET == 0 && read(op + USBSTS) & STS_NOT_READY == 0
        }) {
            return Err("the controller didn't reset");
        }

        // Device slots, and the table of their contexts, with the
        // scratchpad buffers the controller may want as entry 0.
        let slots = (hcs1 & 0xFF).min(MAX_SLOTS);
        write(op + CONFIG, slots);
        let scratchpads = ((hcs2 >> 27) & 0x1F) | ((hcs2 >> 21) & 0x1F) << 5;
        if scratchpads > 0 {
            if scratchpads as usize > PAGE_SIZE / 8 {
                return Err("the controller wants too many scratchpad buffers");
            }
            let array = xhci.pool.page().ok_or("not enough DMA memory")?;
            for i in 0..scratchpads as usize {
                let buffer = xhci
                    .pool
                    .page()
                    .ok_or("not enough DMA memory for scratchpads")?;
                array.write64(i * 8, buffer.bus);
            }
            xhci.dcbaa.write64(0, array.bus);
        }
        write_barrier();
        write64(op + DCBAAP, xhci.dcbaa.bus);
        write64(op + CRCR, xhci.commands.dequeue_pointer());

        // One event ring segment, described by a one-entry table.
        erst.write64(0, xhci.events.page.bus);
        erst.write32(8, RING_TRBS as u32);
        write_barrier();
        let runtime = xhci.runtime;
        write(runtime + ERSTSZ, 1);
        write64(runtime + ERDP, xhci.events.page.bus);
        write64(runtime + ERSTBA, erst.bus);
        write(runtime + IMAN, read(runtime + IMAN) & !0x2); // no interrupts: we poll

        write(op + USBCMD, CMD_RUN);
        if !wait_for(100, || read(op + USBSTS) & STS_HALTED == 0) {
            return Err("the controller didn't start");
        }
        Ok(xhci)
    }

    fn port_register(&self, port: u8) -> usize {
        self.op + PORTSC + 0x10 * (port as usize - 1)
    }

    pub fn port_status(&self, port: u8) -> u32 {
        read(self.port_register(port))
    }

    /// Clear a port's change bits.
    pub fn clear_port_changes(&self, port: u8) {
        let status = self.port_status(port);
        write(
            self.port_register(port),
            (status & PORT_KEEP) | (status & PORT_CHANGES),
        );
    }

    /// Make sure a connected port is enabled (resetting it if it is a USB 2
    /// port: USB 3 ports enable themselves) and return the device's speed.
    pub fn enable_port(&self, port: u8) -> Result<u8> {
        let register = self.port_register(port);
        if read(register) & PORT_ENABLED == 0 {
            write(register, (read(register) & PORT_KEEP) | PORT_RESET);
            if !wait_for(500, || read(register) & PORT_RESET_CHANGE != 0) {
                return Err("port reset didn't finish");
            }
        }
        self.clear_port_changes(port);
        let status = read(register);
        if status & PORT_ENABLED == 0 {
            return Err("the port didn't enable");
        }
        Ok(((status >> 10) & 0xF) as u8)
    }

    pub fn ring_doorbell(&self, slot: u8, target: u8) {
        write_barrier();
        write(self.doorbells + 4 * slot as usize, target as u32);
    }

    /// The next event from the controller, if there is one.
    fn poll_event(&mut self) -> Option<Trb> {
        let events = &mut self.events;
        let at = events.index * 16;
        let control = events.page.read32(at + 12);
        if (control & CYCLE != 0) != events.cycle {
            return None;
        }
        read_barrier();
        let trb = Trb {
            param: events.page.read32(at) as u64 | (events.page.read32(at + 4) as u64) << 32,
            status: events.page.read32(at + 8),
            control,
        };
        events.index += 1;
        if events.index == RING_TRBS {
            events.index = 0;
            events.cycle = !events.cycle;
        }
        let dequeue = events.page.bus + (events.index * 16) as u64;
        write64(self.runtime + ERDP, dequeue | ERDP_BUSY);
        Some(trb)
    }

    /// The next transfer or port event for the main loop: stashed ones
    /// first.
    pub fn next_event(&mut self) -> Option<Trb> {
        if let Some(slot) = self.stash.iter_mut().find(|s| s.is_some()) {
            return slot.take();
        }
        self.poll_event()
    }

    /// Wait up to `ms` for an event `wanted` accepts, stashing transfer
    /// events that aren't it for the main loop.
    fn wait_event(&mut self, ms: u64, wanted: impl Fn(&Trb) -> bool) -> Result<Trb> {
        let deadline = now_ms() + ms;
        loop {
            while let Some(event) = self.poll_event() {
                if wanted(&event) {
                    return Ok(event);
                }
                if event.kind() == TRANSFER_EVENT
                    && let Some(slot) = self.stash.iter_mut().find(|s| s.is_none())
                {
                    *slot = Some(event);
                }
            }
            if now_ms() > deadline {
                return Err("the controller didn't answer");
            }
            libpios::yield_now();
        }
    }

    /// Run a command and wait for it to complete.
    pub fn command(&mut self, trb: Trb) -> Result<Trb> {
        let bus = self.commands.push(trb);
        self.ring_doorbell(0, 0);
        let event = self.wait_event(1000, |e| e.kind() == COMMAND_COMPLETION && e.param == bus)?;
        if event.code() != SUCCESS {
            return Err("a command failed");
        }
        Ok(event)
    }

    pub fn enable_slot(&mut self) -> Result<u8> {
        let event = self.command(Trb {
            control: ENABLE_SLOT << 10,
            ..Trb::default()
        })?;
        Ok(event.slot())
    }

    pub fn disable_slot(&mut self, slot: u8) {
        self.dcbaa.write64(slot as usize * 8, 0);
        let _ = self.command(Trb {
            control: DISABLE_SLOT << 10 | (slot as u32) << 24,
            ..Trb::default()
        });
    }

    /// Give slot `slot` its output device context.
    pub fn set_device_context(&mut self, slot: u8, context: &Page) {
        self.dcbaa.write64(slot as usize * 8, context.bus);
        write_barrier();
    }

    pub fn address_device(&mut self, slot: u8, input: &Page) -> Result<()> {
        self.command(Trb {
            param: input.bus,
            control: ADDRESS_DEVICE << 10 | (slot as u32) << 24,
            ..Trb::default()
        })
        .map(|_| ())
    }

    pub fn evaluate_context(&mut self, slot: u8, input: &Page) -> Result<()> {
        self.command(Trb {
            param: input.bus,
            control: EVALUATE_CONTEXT << 10 | (slot as u32) << 24,
            ..Trb::default()
        })
        .map(|_| ())
    }

    pub fn configure_endpoint(&mut self, slot: u8, input: &Page) -> Result<()> {
        self.command(Trb {
            param: input.bus,
            control: CONFIGURE_ENDPOINT << 10 | (slot as u32) << 24,
            ..Trb::default()
        })
        .map(|_| ())
    }

    /// A control transfer on endpoint 0 of `slot` (whose transfer ring is
    /// `ring`): the 8-byte `setup` packet, then `length` bytes to or from
    /// `buffer` (in when `setup`'s request type says so). Returns once the
    /// status stage completes.
    pub fn control(
        &mut self,
        slot: u8,
        ring: &mut Ring,
        setup: [u8; 8],
        buffer: &Page,
        length: u16,
    ) -> Result<()> {
        let device_to_host = setup[0] & 0x80 != 0;
        let transfer_type = match (length, device_to_host) {
            (0, _) => 0,
            (_, false) => 2,
            (_, true) => 3,
        };
        ring.push(Trb {
            param: u64::from_le_bytes(setup),
            status: 8,
            control: SETUP << 10 | IDT | transfer_type << 16,
        });
        if length > 0 {
            ring.push(Trb {
                param: buffer.bus,
                status: length as u32,
                control: DATA << 10 | if device_to_host { DIR_IN } else { 0 },
            });
        }
        let status_in = length == 0 || !device_to_host;
        let status = ring.push(Trb {
            control: STATUS << 10 | IOC | if status_in { DIR_IN } else { 0 },
            ..Trb::default()
        });
        self.ring_doorbell(slot, 1);
        // The status stage's event says it is done; an error on an earlier
        // stage comes as that stage's event.
        loop {
            let event = self.wait_event(1000, |e| {
                e.kind() == TRANSFER_EVENT && e.slot() == slot && e.endpoint() == 1
            })?;
            match event.code() {
                SUCCESS | SHORT_PACKET if event.param == status => return Ok(()),
                SUCCESS | SHORT_PACKET => {}
                _ => return Err("a control transfer failed"),
            }
        }
    }
}
