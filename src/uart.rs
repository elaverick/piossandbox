//! The kernel's UARTs. The PL011 driver itself is shared with the console
//! server (`pios-pl011`); this is how the kernel reaches its UARTs, through
//! its linear map.

pub use pios_pl011::{BAUD_RATE, Pl011};

use crate::addr::PhysAddr;
use crate::timer::Deadline;

/// The UART whose registers are at physical address `base`.
///
/// # Safety
///
/// There must be a PL011 at `base`, within the kernel map's devices.
pub unsafe fn at(base: PhysAddr) -> Pl011 {
    // SAFETY: the caller promises a PL011 is there, and the kernel map
    // covers the board's devices for good.
    unsafe { Pl011::new(base.to_virt().as_usize()) }
}

/// Set up `uart` for `baud` baud, 8n1, given its reference clock.
pub fn init(uart: Pl011, clock_hz: u32, baud: u32) {
    let deadline = Deadline::after_us(10_000);
    uart.init(clock_hz, baud, || deadline.expired());
}
