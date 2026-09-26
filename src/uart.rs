//! UART0: the ARM PL011 UART, wired to GPIO 14 (TXD) and 15 (RXD), which are
//! pins 8 and 10 on the 40-pin header.

use core::fmt;

use crate::gpio::{self, Function, Pull};
use crate::mailbox;
use crate::mmio::{self, UART0_BASE};

const DR: usize = UART0_BASE + 0x00; // Data
const FR: usize = UART0_BASE + 0x18; // Flags
const IBRD: usize = UART0_BASE + 0x24; // Integer baud rate divisor
const FBRD: usize = UART0_BASE + 0x28; // Fractional baud rate divisor
const LCRH: usize = UART0_BASE + 0x2C; // Line control
const CR: usize = UART0_BASE + 0x30; // Control
const IMSC: usize = UART0_BASE + 0x38; // Interrupt mask set/clear
const ICR: usize = UART0_BASE + 0x44; // Interrupt clear

const FR_BUSY: u32 = 1 << 3;
const FR_RXFE: u32 = 1 << 4; // Receive FIFO empty
const FR_TXFF: u32 = 1 << 5; // Transmit FIFO full

const LCRH_FEN: u32 = 1 << 4; // Enable FIFOs
const LCRH_WLEN_8: u32 = 0b11 << 5; // 8 data bits

const CR_UARTEN: u32 = 1 << 0;
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;

pub const BAUD_RATE: u32 = 115_200;

/// Clock we ask the firmware to run the UART at (as the OSDev tutorial does).
const REQUESTED_CLOCK_HZ: u32 = 3_000_000;
/// The Pi 4 firmware's default UART clock, used if the mailbox call fails.
const DEFAULT_CLOCK_HZ: u32 = 48_000_000;

/// Configure UART0 for 115200 baud, 8 data bits, no parity, 1 stop bit.
/// Returns the UART reference clock in Hz.
pub fn init() -> u32 {
    // Disable the UART, let any in-flight character finish, and flush the
    // transmit FIFO before reprogramming it (PL011 TRM section 3.3.8).
    mmio::write(CR, 0);
    while mmio::read(FR) & FR_BUSY != 0 {
        core::hint::spin_loop();
    }
    mmio::write(LCRH, 0);

    // Route GPIO 14/15 to UART0 (ALT0) with no pull resistors. The firmware
    // gives these pins to the mini UART by default on a Pi 4.
    for pin in [14, 15] {
        gpio::set_function(pin, Function::Alt0);
        gpio::set_pull(pin, Pull::None);
    }

    // Clear any pending interrupts.
    mmio::write(ICR, 0x7FF);

    // Ask the firmware to set the UART clock, and use whatever rate it
    // actually reports back to compute the divisor.
    let mut clock = [mailbox::CLOCK_UART, REQUESTED_CLOCK_HZ, 0];
    let clock_hz = match mailbox::property(mailbox::TAG_SET_CLOCK_RATE, &mut clock) {
        Some(()) if clock[1] != 0 => clock[1],
        _ => DEFAULT_CLOCK_HZ,
    };

    // Divisor = clock / (16 * baud), as a 16.6 fixed-point number, rounded.
    let divisor = (clock_hz as u64 * 4 + BAUD_RATE as u64 / 2) / BAUD_RATE as u64;
    mmio::write(IBRD, (divisor >> 6) as u32);
    mmio::write(FBRD, (divisor & 0x3F) as u32);

    // 8n1 with FIFOs enabled.
    mmio::write(LCRH, LCRH_FEN | LCRH_WLEN_8);

    // All UART interrupts disabled: we poll for now.
    mmio::write(IMSC, 0);

    mmio::write(CR, CR_UARTEN | CR_TXE | CR_RXE);

    clock_hz
}

/// Send one byte, waiting for room in the transmit FIFO.
pub fn putc(byte: u8) {
    while mmio::read(FR) & FR_TXFF != 0 {
        core::hint::spin_loop();
    }
    mmio::write(DR, byte as u32);
}

/// Receive one byte, waiting until one arrives.
pub fn getc() -> u8 {
    while mmio::read(FR) & FR_RXFE != 0 {
        core::hint::spin_loop();
    }
    mmio::read(DR) as u8
}

/// Send a string, translating `\n` into `\r\n` for serial terminals.
pub fn puts(s: &str) {
    for byte in s.bytes() {
        if byte == b'\n' {
            putc(b'\r');
        }
        putc(byte);
    }
}

/// Zero-sized handle so `core::fmt` machinery can write to the UART.
pub struct Uart;

impl fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        puts(s);
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use fmt::Write;
    // Writing to the UART cannot fail.
    let _ = Uart.write_fmt(args);
}

/// Print to UART0.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::uart::_print(format_args!($($arg)*)));
}

/// Print to UART0, followed by a newline.
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
