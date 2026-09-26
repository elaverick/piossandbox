//! The ARM generic timer: a 64-bit counter running at a fixed frequency
//! (54 MHz on the Pi 4 and 5), readable at any exception level.

use core::arch::asm;

/// Used if the firmware did not program CNTFRQ_EL0.
const DEFAULT_FREQUENCY_HZ: u64 = 54_000_000;

/// Counter ticks per second.
pub fn frequency() -> u64 {
    let freq: u64;
    // SAFETY: reading CNTFRQ_EL0 has no side effects.
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) freq, options(nomem, nostack)) };
    if freq == 0 {
        DEFAULT_FREQUENCY_HZ
    } else {
        freq
    }
}

/// The current counter value.
pub fn ticks() -> u64 {
    let ticks: u64;
    // SAFETY: reading CNTPCT_EL0 has no side effects. The `isb` stops the
    // read from being speculated ahead of earlier instructions.
    unsafe { asm!("isb", "mrs {}, cntpct_el0", out(reg) ticks, options(nomem, nostack)) };
    ticks
}

/// A point in time after which an operation should give up.
pub struct Deadline(u64);

impl Deadline {
    pub fn after_us(us: u64) -> Self {
        Deadline(ticks() + us * frequency() / 1_000_000)
    }

    pub fn expired(&self) -> bool {
        ticks() >= self.0
    }
}
