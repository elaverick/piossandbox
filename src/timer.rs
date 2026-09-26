//! The ARM generic timer: a 64-bit counter running at a fixed frequency
//! (54 MHz on the Pi 4 and 5), readable at any exception level.

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

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

/// The periodic tick: the EL1 physical timer, interrupt ID 30 (PPI 14) on
/// both the Pi 4 and Pi 5.
pub const TICK_IRQ: u32 = 30;
pub const TICK_HZ: u64 = 100;

static TICKS: AtomicU64 = AtomicU64::new(0);
static TICK_INTERVAL: AtomicU64 = AtomicU64::new(0);

/// Number of timer ticks since `start_tick`.
pub fn tick_count() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// CNTKCTL_EL1: user programs may read the virtual counter (CNTVCT_EL0)
/// and its frequency, as on Linux, but not use the timers.
const CNTKCTL_EL0VCTEN: u64 = 1 << 1;

/// Start the periodic tick. Register `handle_tick` for `TICK_IRQ` first.
pub fn start_tick() {
    let interval = frequency() / TICK_HZ;
    TICK_INTERVAL.store(interval, Ordering::Relaxed);
    // SAFETY: programming our own EL1 physical timer has no other effect;
    // letting user programs read the time exposes nothing else.
    unsafe {
        asm!("msr cntkctl_el1, {}", in(reg) CNTKCTL_EL0VCTEN, options(nomem, nostack));
        asm!("msr cntp_cval_el0, {}", in(reg) ticks() + interval, options(nomem, nostack));
        asm!("msr cntp_ctl_el0, {}", in(reg) 1u64, options(nomem, nostack)); // enabled, unmasked
    }
}

/// Interrupt handler for the tick.
pub fn handle_tick() {
    let interval = TICK_INTERVAL.load(Ordering::Relaxed);
    let mut next: u64;
    // SAFETY: see `start_tick`.
    unsafe { asm!("mrs {}, cntp_cval_el0", out(reg) next, options(nomem, nostack)) };
    // Schedule the next tick relative to the last one so the rate doesn't
    // drift, unless we have fallen behind.
    next += interval;
    if next <= ticks() {
        next = ticks() + interval;
    }
    // SAFETY: see `start_tick`.
    unsafe { asm!("msr cntp_cval_el0, {}", in(reg) next, options(nomem, nostack)) };
    // Only this handler writes TICKS, so a plain load and store is enough.
    TICKS.store(tick_count() + 1, Ordering::Relaxed);
}
