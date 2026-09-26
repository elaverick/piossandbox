//! Driver for the ARM PL011 UART, shared by the kernel (for its own
//! messages) and the user-space console server. The Pi 4 has several
//! (UART0 is on GPIO 14/15); the Pi 5 has one in the BCM2712 for its debug
//! connector and more in the RP1 chip, including UART0 on GPIO 14/15.

#![no_std]

const DR: usize = 0x00; // Data
const FR: usize = 0x18; // Flags
const IBRD: usize = 0x24; // Integer baud rate divisor
const FBRD: usize = 0x28; // Fractional baud rate divisor
const LCRH: usize = 0x2C; // Line control
const CR: usize = 0x30; // Control
const IFLS: usize = 0x34; // Interrupt FIFO level select
const IMSC: usize = 0x38; // Interrupt mask set/clear
const ICR: usize = 0x44; // Interrupt clear

const FR_BUSY: u32 = 1 << 3;
const FR_RXFE: u32 = 1 << 4; // Receive FIFO empty
const FR_TXFF: u32 = 1 << 5; // Transmit FIFO full

const LCRH_FEN: u32 = 1 << 4; // Enable FIFOs
const LCRH_WLEN_8: u32 = 0b11 << 5; // 8 data bits

const CR_UARTEN: u32 = 1 << 0;
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;
const INT_RX: u32 = 1 << 4; // Receive FIFO reached its trigger level
const INT_RT: u32 = 1 << 6; // Receive timeout: data waiting, line idle

const CR_RTSEN: u32 = 1 << 14; // Hardware (RTS) flow control
const CR_CTSEN: u32 = 1 << 15; // Hardware (CTS) flow control

pub const BAUD_RATE: u32 = 115_200;

/// A PL011 UART.
#[derive(Clone, Copy)]
pub struct Pl011 {
    /// Where its registers are mapped.
    base: usize,
}

impl Pl011 {
    /// The UART whose registers are mapped (as device memory) at `base`.
    ///
    /// # Safety
    ///
    /// A PL011's registers must be mapped at `base` for as long as the
    /// `Pl011` (or a copy) is used, and nothing must depend on them keeping
    /// their values (the driver may reprogram the UART).
    pub const unsafe fn new(base: usize) -> Pl011 {
        Pl011 { base }
    }

    /// Where the registers are mapped.
    pub fn base(&self) -> usize {
        self.base
    }

    fn read(&self, register: usize) -> u32 {
        // SAFETY: `new`'s caller promised the registers are mapped, and
        // every offset used is a PL011 register.
        unsafe { core::ptr::read_volatile((self.base + register) as *const u32) }
    }

    fn write(&self, register: usize, value: u32) {
        // SAFETY: as for `read`.
        unsafe { core::ptr::write_volatile((self.base + register) as *mut u32, value) }
    }

    /// Configure the UART for `baud` baud, 8 data bits, no parity, 1 stop
    /// bit, given its reference clock.
    /// Gives up waiting for the transmitter to go idle when `expired()`.
    pub fn init(&self, clock_hz: u32, baud: u32, mut expired: impl FnMut() -> bool) {
        // Let anything the firmware was sending finish (bounded, in case the
        // UART is stuck), then disable the UART and flush the transmit FIFO
        // before reprogramming it (PL011 TRM section 3.3.8).
        while self.read(FR) & FR_BUSY != 0 && !expired() {
            core::hint::spin_loop();
        }
        self.write(CR, 0);
        self.write(LCRH, 0);

        // Clear any pending interrupts.
        self.write(ICR, 0x7FF);

        // Divisor = clock / (16 * baud), as a 16.6 fixed-point number, rounded.
        let divisor = (clock_hz as u64 * 4 + baud as u64 / 2) / baud as u64;
        self.write(IBRD, (divisor >> 6) as u32);
        self.write(FBRD, (divisor & 0x3F) as u32);

        // 8n1 with FIFOs enabled.
        self.write(LCRH, LCRH_FEN | LCRH_WLEN_8);

        // All UART interrupts disabled: we poll for now. (A 1 bit in IMSC
        // enables an interrupt; the OSDev tutorial gets this backwards.)
        self.write(IMSC, 0);

        self.write(CR, CR_UARTEN | CR_TXE | CR_RXE);
    }

    /// Has something (i.e. the firmware) already enabled this UART and set a
    /// baud rate?
    pub fn is_configured(&self) -> bool {
        self.read(CR) & CR_UARTEN != 0 && self.read(IBRD) != 0
    }

    /// Make sure transmit and receive are enabled, keeping the existing baud
    /// rate and line settings. Hardware flow control is turned off: with
    /// only TX, RX and GND wired up, CTS flow control would stall output.
    pub fn enable(&self) {
        let cr = self.read(CR) & !(CR_RTSEN | CR_CTSEN);
        self.write(CR, cr | CR_UARTEN | CR_TXE | CR_RXE);
    }

    /// Raise an interrupt when data arrives: as soon as the receive FIFO is
    /// 1/8 full, or when anything has been waiting for 32 bit periods.
    pub fn enable_rx_interrupt(&self) {
        self.write(IFLS, 0); // receive trigger at 1/8 full
        self.write(ICR, INT_RX | INT_RT);
        self.write(IMSC, INT_RX | INT_RT);
    }

    /// Stop raising receive interrupts, leaving data in the FIFO.
    pub fn disable_rx_interrupt(&self) {
        self.write(IMSC, 0);
    }

    /// Acknowledge the receive interrupts. Reading the FIFO until it is
    /// empty stops the conditions that raise them.
    pub fn clear_rx_interrupt(&self) {
        self.write(ICR, INT_RX | INT_RT);
    }

    /// Send one byte, waiting for room in the transmit FIFO.
    pub fn putc(&self, byte: u8) {
        while self.read(FR) & FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        self.write(DR, byte as u32);
    }

    /// Receive one byte, if one is waiting.
    pub fn try_getc(&self) -> Option<u8> {
        if self.read(FR) & FR_RXFE != 0 {
            None
        } else {
            Some(self.read(DR) as u8)
        }
    }
}
