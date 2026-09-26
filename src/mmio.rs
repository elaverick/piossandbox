//! Memory-mapped I/O helpers and the BCM2711 peripheral address map.

/// Base of the peripheral window as seen by the ARM cores on the BCM2711
/// (Raspberry Pi 4) in the default "low peripheral" mode.
///
/// The Pi 2/3 use 0x3F00_0000 and the Pi 1/Zero 0x2000_0000.
pub const MMIO_BASE: usize = 0xFE00_0000;

/// GPIO controller.
pub const GPIO_BASE: usize = MMIO_BASE + 0x20_0000;

/// UART0 (ARM PL011).
pub const UART0_BASE: usize = MMIO_BASE + 0x20_1000;

/// VideoCore mailbox.
pub const MBOX_BASE: usize = MMIO_BASE + 0xB880;

/// Write a 32-bit device register.
#[inline(always)]
pub fn write(addr: usize, value: u32) {
    // SAFETY: callers only pass addresses of BCM2711 device registers, which
    // are always mapped (the MMU is off) and 4-byte aligned.
    unsafe { core::ptr::write_volatile(addr as *mut u32, value) }
}

/// Read a 32-bit device register.
#[inline(always)]
pub fn read(addr: usize) -> u32 {
    // SAFETY: see `write`.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

/// Data synchronisation barrier: all earlier memory accesses complete before
/// any later ones start.
#[inline(always)]
pub fn dsb() {
    // SAFETY: a barrier has no effect other than ordering memory accesses.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) }
}
