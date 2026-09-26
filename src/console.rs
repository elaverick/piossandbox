//! The system console: output goes to every registered UART and to the
//! display (if there is one), and input is taken from whichever UART has a
//! byte waiting.

use core::cell::UnsafeCell;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use crate::uart::Pl011;

const MAX_UARTS: usize = 2;

/// Base addresses of the console UARTs (0 marks an empty slot), and the
/// interrupt ID of each (0 if it is polled instead).
///
/// Only plain atomic loads and stores are used: with the MMU off, memory is
/// Device memory, where the exclusive-access instructions behind atomic
/// read-modify-write operations are not guaranteed to work.
static UARTS: [AtomicUsize; MAX_UARTS] = [const { AtomicUsize::new(0) }; MAX_UARTS];
static UART_IRQS: [AtomicU32; MAX_UARTS] = [const { AtomicU32::new(0) }; MAX_UARTS];

/// Add an (already initialised) UART to the console. If `irq` is given, its
/// input arrives by interrupt once `enable_interrupts` is called; otherwise
/// `getc` polls it.
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
        .map(|(base, irq)| (Pl011::new(base), irq))
}

fn uarts() -> impl Iterator<Item = Pl011> {
    slots().map(|(uart, _)| uart)
}

/// Switch interrupt-capable console UARTs to interrupt-driven input.
pub fn enable_interrupts() {
    for (uart, irq) in slots() {
        if irq != 0 {
            crate::irq::register(irq, uart_interrupt);
            uart.enable_rx_interrupt();
        }
    }
}

/// Set when input interrupts were switched off because the buffer filled.
static INPUT_PAUSED: AtomicBool = AtomicBool::new(false);

/// Interrupt handler: move received bytes into the input buffer.
///
/// If the buffer fills up (say a paste arrives while the display is busy
/// scrolling), the rest stays in the UART's FIFO and its receive interrupt
/// is switched off until `getc` makes room, rather than dropping bytes.
fn uart_interrupt() {
    for (uart, irq) in slots() {
        if irq != 0 && !uart.drain_rx(|| !INPUT.is_full(), |byte| INPUT.push(byte)) {
            uart.disable_rx_interrupt();
            INPUT_PAUSED.store(true, Ordering::Release);
        }
    }
}

/// Undo `uart_interrupt` switching input off, once there is room again.
fn resume_input() {
    if INPUT_PAUSED.load(Ordering::Acquire) && INPUT.len() < INPUT_SIZE / 2 {
        crate::irq::without_interrupts(|| {
            INPUT_PAUSED.store(false, Ordering::Release);
            for (uart, irq) in slots() {
                if irq != 0 {
                    uart.enable_rx_interrupt();
                }
            }
            // A UART may not interrupt again for data already sitting in a
            // full FIFO (QEMU's never does), so collect it now.
            uart_interrupt();
        });
    }
}

/// Bytes received by interrupt, waiting for `getc`. The interrupt handler
/// only moves `head` and `getc` only moves `tail`, so no locking is needed.
struct InputBuffer {
    bytes: UnsafeCell<[u8; INPUT_SIZE]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

const INPUT_SIZE: usize = 1024;

// SAFETY: single producer (the interrupt handler) and single consumer
// (`getc`); each slot is written before `head` publishes it and read before
// `tail` releases it.
unsafe impl Sync for InputBuffer {}

static INPUT: InputBuffer = InputBuffer {
    bytes: UnsafeCell::new([0; INPUT_SIZE]),
    head: AtomicUsize::new(0),
    tail: AtomicUsize::new(0),
};

impl InputBuffer {
    fn len(&self) -> usize {
        self.head
            .load(Ordering::Acquire)
            .wrapping_sub(self.tail.load(Ordering::Acquire))
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn is_full(&self) -> bool {
        self.len() >= INPUT_SIZE
    }

    /// Add a byte; dropped if the buffer is full.
    fn push(&self, byte: u8) {
        if self.is_full() {
            return;
        }
        let head = self.head.load(Ordering::Relaxed);
        // SAFETY: the consumer doesn't touch this slot until `head` moves
        // past it.
        unsafe { (*self.bytes.get())[head % INPUT_SIZE] = byte };
        self.head.store(head.wrapping_add(1), Ordering::Release);
    }

    fn pop(&self) -> Option<u8> {
        let tail = self.tail.load(Ordering::Relaxed);
        if tail == self.head.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: the producer doesn't reuse this slot until `tail` moves
        // past it.
        let byte = unsafe { (*self.bytes.get())[tail % INPUT_SIZE] };
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(byte)
    }
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
        if let Some(byte) = INPUT.pop() {
            resume_input();
            return byte;
        }
        // Collect anything no interrupt was raised for. A UART only
        // interrupts on a change, so data that arrived before interrupts
        // were enabled, or while they were paused, could otherwise wait
        // forever. This runs at least on every timer tick.
        crate::irq::without_interrupts(uart_interrupt);
        if !INPUT.is_empty() {
            continue;
        }
        let mut polled = false;
        for (uart, irq) in slots() {
            if irq == 0 {
                polled = true;
                if let Some(byte) = uart.try_getc() {
                    return byte;
                }
            }
        }
        if polled {
            core::hint::spin_loop();
        } else {
            // Everything is interrupt-driven: sleep until an interrupt (a
            // key, or at worst the next timer tick).
            // SAFETY: `wfi` just idles the core until an interrupt.
            unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
        }
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
