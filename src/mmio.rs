//! Memory-mapped I/O helpers.

/// Write a 32-bit device register.
#[inline(always)]
pub fn write(addr: usize, value: u32) {
    // SAFETY: callers only pass addresses of device registers that exist on
    // the board we detected, all mapped as Device memory (see mmu.rs).
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
