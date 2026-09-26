//! Data cache maintenance, for memory shared with devices that don't see the
//! CPU's caches (on the Pi, the GPU).

use core::arch::asm;

/// Size of the smallest data cache line, from CTR_EL0.
pub fn line_size() -> usize {
    let ctr: u64;
    // SAFETY: reading CTR_EL0 has no side effects.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    4 << ((ctr >> 16) & 0xF)
}

/// Apply a `dc` operation to every line covering `[start, start + len)`.
macro_rules! for_each_line {
    ($op:literal, $start:expr, $len:expr) => {{
        let line = line_size();
        let end = $start + $len;
        let mut addr = $start & !(line - 1);
        while addr < end {
            // SAFETY: cache maintenance by address doesn't change memory
            // contents as seen through the caches (clean) or only discards
            // lines the caller has said it doesn't need (invalidate).
            unsafe { asm!(concat!("dc ", $op, ", {}"), in(reg) addr, options(nostack)) };
            addr += line;
        }
        // SAFETY: a barrier only orders memory accesses.
        unsafe { asm!("dsb sy", options(nostack)) };
    }};
}

/// Write any cached changes in the range out to memory, so a device reading
/// memory sees them.
pub fn clean(start: usize, len: usize) {
    for_each_line!("cvac", start, len);
}

/// Discard cached copies of the range, so the next reads fetch what a device
/// wrote to memory. Any unwritten changes in those lines are lost: the range
/// must cover whole cache lines that nothing else shares.
pub fn invalidate(start: usize, len: usize) {
    for_each_line!("ivac", start, len);
}

/// Write any cached changes in the range out as far as the point where
/// instruction fetches see them (for code written through the data cache).
pub fn clean_to_unification(start: usize, len: usize) {
    for_each_line!("cvau", start, len);
}

/// Discard all instruction cache contents, so newly written code is
/// fetched from memory.
pub fn invalidate_instruction_cache() {
    // SAFETY: invalidating the instruction cache only forces refetches.
    unsafe { asm!("dsb ish", "ic iallu", "dsb ish", "isb", options(nostack)) };
}
