//! The system console: output goes to every registered UART and to the
//! display (if there is one), and input is taken from whichever UART has a
//! byte waiting.

use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::uart::Pl011;

const MAX_UARTS: usize = 2;

/// Base addresses of the console UARTs; 0 marks an empty slot.
///
/// Only plain atomic loads and stores are used: with the MMU off, memory is
/// Device memory, where the exclusive-access instructions behind atomic
/// read-modify-write operations are not guaranteed to work.
static UARTS: [AtomicUsize; MAX_UARTS] = [const { AtomicUsize::new(0) }; MAX_UARTS];

/// Add an (already initialised) UART to the console.
pub fn add(uart: Pl011) {
    // Only core 0 runs for now, so there is no race here.
    if let Some(slot) = UARTS.iter().find(|s| s.load(Ordering::Relaxed) == 0) {
        slot.store(uart.base(), Ordering::Relaxed);
    }
}

fn uarts() -> impl Iterator<Item = Pl011> {
    UARTS
        .iter()
        .map(|slot| slot.load(Ordering::Relaxed))
        .filter(|&base| base != 0)
        .map(Pl011::new)
}

/// Send one byte to every console UART and the display.
pub fn putc(byte: u8) {
    for uart in uarts() {
        uart.putc(byte);
    }
    crate::framebuffer::putc(byte);
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

/// Wait for a byte from any console UART.
pub fn getc() -> u8 {
    loop {
        for uart in uarts() {
            if let Some(byte) = uart.try_getc() {
                return byte;
            }
        }
        core::hint::spin_loop();
    }
}

/// Zero-sized handle so `core::fmt` machinery can write to the console.
pub struct Console;

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        puts(s);
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use fmt::Write;
    // Writing to the console cannot fail.
    let _ = Console.write_fmt(args);
}

/// Print to the console.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::console::_print(format_args!($($arg)*)));
}

/// Print to the console, followed by a newline.
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
