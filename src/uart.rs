//! Driver for the ARM PL011 UART. The Pi 4 has several (UART0 is on GPIO
//! 14/15); the Pi 5 has one in the BCM2712 for its debug connector and more
//! in the RP1 chip, including UART0 on GPIO 14/15.

use crate::mmio;
use crate::timer::Deadline;

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

/// A PL011 UART at `base`.
#[derive(Clone, Copy)]
pub struct Pl011 {
    base: usize,
}

impl Pl011 {
    pub const fn new(base: usize) -> Self {
        Pl011 { base }
    }

    pub fn base(&self) -> usize {
        self.base
    }

    /// Configure the UART for `baud` baud, 8 data bits, no parity, 1 stop
    /// bit, given its reference clock.
    pub fn init(&self, clock_hz: u32, baud: u32) {
        // Let anything the firmware was sending finish (bounded, in case the
        // UART is stuck), then disable the UART and flush the transmit FIFO
        // before reprogramming it (PL011 TRM section 3.3.8).
        let deadline = Deadline::after_us(10_000);
        while mmio::read(self.base + FR) & FR_BUSY != 0 && !deadline.expired() {
            core::hint::spin_loop();
        }
        mmio::write(self.base + CR, 0);
        mmio::write(self.base + LCRH, 0);

        // Clear any pending interrupts.
        mmio::write(self.base + ICR, 0x7FF);

        // Divisor = clock / (16 * baud), as a 16.6 fixed-point number, rounded.
        let divisor = (clock_hz as u64 * 4 + baud as u64 / 2) / baud as u64;
        mmio::write(self.base + IBRD, (divisor >> 6) as u32);
        mmio::write(self.base + FBRD, (divisor & 0x3F) as u32);

        // 8n1 with FIFOs enabled.
        mmio::write(self.base + LCRH, LCRH_FEN | LCRH_WLEN_8);

        // All UART interrupts disabled: we poll for now. (A 1 bit in IMSC
        // enables an interrupt; the OSDev tutorial gets this backwards.)
        mmio::write(self.base + IMSC, 0);

        mmio::write(self.base + CR, CR_UARTEN | CR_TXE | CR_RXE);
    }

    /// Has something (i.e. the firmware) already enabled this UART and set a
    /// baud rate?
    pub fn is_configured(&self) -> bool {
        mmio::read(self.base + CR) & CR_UARTEN != 0 && mmio::read(self.base + IBRD) != 0
    }

    /// Make sure transmit and receive are enabled, keeping the existing baud
    /// rate and line settings. Hardware flow control is turned off: with
    /// only TX, RX and GND wired up, CTS flow control would stall output.
    pub fn enable(&self) {
        let cr = mmio::read(self.base + CR) & !(CR_RTSEN | CR_CTSEN);
        mmio::write(self.base + CR, cr | CR_UARTEN | CR_TXE | CR_RXE);
    }

    /// Raise an interrupt when data arrives: as soon as the receive FIFO is
    /// 1/8 full, or when anything has been waiting for 32 bit periods.
    pub fn enable_rx_interrupt(&self) {
        mmio::write(self.base + IFLS, 0); // receive trigger at 1/8 full
        mmio::write(self.base + ICR, INT_RX | INT_RT);
        mmio::write(self.base + IMSC, INT_RX | INT_RT);
    }

    /// Stop raising receive interrupts, leaving data in the FIFO.
    pub fn disable_rx_interrupt(&self) {
        mmio::write(self.base + IMSC, 0);
    }

    /// Interrupt handler body: move received bytes to `push` while
    /// `has_room()`. Returns false if bytes were left in the FIFO for lack
    /// of room.
    pub fn drain_rx(&self, has_room: impl Fn() -> bool, mut push: impl FnMut(u8)) -> bool {
        mmio::write(self.base + ICR, INT_RX | INT_RT);
        while mmio::read(self.base + FR) & FR_RXFE == 0 {
            if !has_room() {
                return false;
            }
            push(mmio::read(self.base + DR) as u8);
        }
        true
    }

    /// Send one byte, waiting for room in the transmit FIFO.
    pub fn putc(&self, byte: u8) {
        while mmio::read(self.base + FR) & FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        mmio::write(self.base + DR, byte as u32);
    }

    /// Receive one byte, if one is waiting.
    pub fn try_getc(&self) -> Option<u8> {
        if mmio::read(self.base + FR) & FR_RXFE != 0 {
            None
        } else {
            Some(mmio::read(self.base + DR) as u8)
        }
    }
}
