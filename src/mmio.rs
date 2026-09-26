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

// `probe_read32(addr) -> u64`: the 32-bit value at `addr`, or u64::MAX if
// reading it causes an external abort (nothing answers there). The
// exception handler knows the one instruction that may fault and skips to
// the end, leaving x0 as it was.
core::arch::global_asm!(
    ".global probe_read32",
    ".global probe_read32_load",
    ".global probe_read32_done",
    "probe_read32:",
    "    mov x1, x0",
    "    mov x0, #-1",
    "probe_read32_load:",
    "    ldr w2, [x1]",
    "    mov x0, x2",
    "probe_read32_done:",
    "    ret",
);

unsafe extern "C" {
    fn probe_read32(addr: usize) -> u64;
    static probe_read32_load: u8;
    static probe_read32_done: u8;
}

/// Read a 32-bit device register that may not be there: `None` if the
/// read is aborted (as it is for an address nothing decodes, in QEMU, or a
/// controller that is powered off).
pub fn probe_read(addr: usize) -> Option<u32> {
    // SAFETY: as for `read`, except that the address may have nothing
    // behind it, which the exception handler turns into u64::MAX.
    let value = unsafe { probe_read32(addr) };
    u32::try_from(value).ok()
}

/// If `pc` is the probing load, where to resume after it faulted.
pub fn probe_fixup(pc: u64) -> Option<u64> {
    let load = &raw const probe_read32_load as u64;
    (pc == load).then_some(&raw const probe_read32_done as u64)
}
