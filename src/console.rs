//! The kernel's console: output goes to every registered UART and to the
//! display (if there is one). The kernel reads no input: once it starts
//! `init`, the UARTs' input and the display belong to the console server,
//! and the kernel only writes its own messages to the UARTs.

use core::fmt;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::uart::Pl011;

const MAX_UARTS: usize = 2;

/// Base addresses of the console UARTs (0 marks an empty slot), and the
/// interrupt ID of each (0 if it has to be polled). Only changed during
/// setup, before interrupts are enabled.
static UARTS: [AtomicUsize; MAX_UARTS] = [const { AtomicUsize::new(0) }; MAX_UARTS];
static UART_IRQS: [AtomicU32; MAX_UARTS] = [const { AtomicU32::new(0) }; MAX_UARTS];

/// Add an (already initialised) UART to the console, with its interrupt if
/// it has one (for the console server).
pub fn add(uart: Pl011, irq: Option<u32>) {
    // Only core 0 runs for now, so there is no race here.
    if let Some(slot) = (0..MAX_UARTS).find(|&i| UARTS[i].load(Ordering::Relaxed) == 0) {
        UART_IRQS[slot].store(irq.unwrap_or(0), Ordering::Relaxed);
        UARTS[slot].store(uart.base(), Ordering::Relaxed);
    }
}

fn slots() -> impl Iterator<Item = (Pl011, u32)> {
    (0..MAX_UARTS)
        .map(|i| {
            (
                UARTS[i].load(Ordering::Relaxed),
                UART_IRQS[i].load(Ordering::Relaxed),
            )
        })
        .filter(|&(base, _)| base != 0)
        // SAFETY: `add` stored the base of an initialised UART, which the
        // kernel map covers for good.
        .map(|(base, irq)| (unsafe { Pl011::new(base) }, irq))
}

fn uarts() -> impl Iterator<Item = Pl011> {
    slots().map(|(uart, _)| uart)
}

/// The console UARTs' physical addresses and interrupts, to hand to the
/// console server. (The kernel still writes its own messages to them.)
pub fn uarts_to_hand_over() -> impl Iterator<Item = (crate::addr::PhysAddr, Option<u32>)> {
    slots().map(|(uart, irq)| {
        let base = crate::addr::VirtAddr::new(uart.base())
            .to_phys()
            .expect("UARTs are in the linear map");
        (base, (irq != 0).then_some(irq))
    })
}

/// Send one byte to every console UART and the display.
pub fn putc(byte: u8) {
    // Without interrupts, so another thread can't cut in half way (and the
    // display, which refuses re-entry, doesn't miss its output).
    crate::irq::without_interrupts(|| {
        for uart in uarts() {
            uart.putc(byte);
        }
        crate::framebuffer::putc(byte);
    });
}

/// Send a string, translating `\n` into `\r\n` for serial terminals.
pub fn puts(s: &str) {
    write_bytes(s.as_bytes());
}

/// Send bytes (which needn't be UTF-8), translating `\n` into `\r\n`.
pub fn write_bytes(bytes: &[u8]) {
    for &byte in bytes {
        if byte == b'\n' {
            putc(b'\r');
        }
        putc(byte);
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
