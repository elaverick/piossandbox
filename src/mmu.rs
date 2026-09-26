//! The memory map. boot.s builds it and turns the MMU on before any Rust
//! code runs; this module describes it and makes later changes.
//!
//! Virtual addresses equal physical ones (an identity map), 39 bits wide,
//! with 4 KiB pages:
//!
//! | Range                         | Mapped as                     | What        |
//! |-------------------------------|-------------------------------|-------------|
//! | 0 - 1 GiB                     | Normal, write-back (2 MiB)    | RAM, incl. the GPU's share |
//! | 3 - 4 GiB                     | Device                        | Pi 4 peripherals |
//! | 0x10_0000_0000 + 2 GiB        | Device                        | Pi 5 PCIe controllers, SoC peripherals |
//! | 0x1F_0000_0000 + 1 GiB        | Device                        | Pi 5 RP1 (through PCIe) |
//!
//! Everything else is unmapped, so a stray access faults instead of reaching
//! something unexpected. The framebuffer is later switched to Normal
//! non-cacheable by `map_non_cacheable`.

use core::arch::asm;

use crate::cache;

const BLOCK_SIZE: usize = 2 << 20;
const L2_COVERS: usize = 512 * BLOCK_SIZE;

/// RAM is mapped from 0 up to here.
pub const MAPPED_RAM_END: usize = L2_COVERS;

// Block descriptors (see boot.s).
const DESCRIPTOR_TYPE_MASK: u64 = 0b11;
const ATTR_INDEX_MASK: u64 = 0b111 << 2;
const ATTR_NON_CACHEABLE: u64 = 2 << 2;
const EXECUTE_NEVER: u64 = (1 << 53) | (1 << 54);

unsafe extern "C" {
    /// The level 2 table covering the first GiB, built by boot.s.
    static mut page_table_l2: [u64; 512];
}

const SCTLR_M: u64 = 1 << 0;
const SCTLR_C: u64 = 1 << 2;
const SCTLR_I: u64 = 1 << 12;

/// Are the MMU and both caches on?
pub fn enabled() -> bool {
    let sctlr: u64;
    // SAFETY: reading SCTLR_EL1 has no side effects.
    unsafe { asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack)) };
    let all = SCTLR_M | SCTLR_C | SCTLR_I;
    sctlr & all == all
}

/// Map `[start, start + len)` as Normal non-cacheable memory (writes may be
/// combined but are never held in the cache), for memory the GPU reads
/// directly. Works in whole 2 MiB blocks within the first GiB; returns false
/// if the range is outside it.
pub fn map_non_cacheable(start: usize, len: usize) -> bool {
    let end = start + len;
    if len == 0 || end > L2_COVERS {
        return false;
    }

    // Write back and drop anything cached for the range while it is still
    // mapped cacheable; afterwards the cache must hold nothing for it.
    cache::clean_invalidate(start, len);

    let table = &raw mut page_table_l2;
    for index in start / BLOCK_SIZE..end.div_ceil(BLOCK_SIZE) {
        // SAFETY: `index` < 512; the table is only changed here, by the
        // primary core, and nothing else uses the range being changed.
        unsafe {
            let entry = (&raw mut (*table)[index]).read_volatile();
            if entry & DESCRIPTOR_TYPE_MASK == 0 {
                continue;
            }
            let new = (entry & !ATTR_INDEX_MASK) | ATTR_NON_CACHEABLE | EXECUTE_NEVER;
            // Break-before-make: the architecture requires the old entry to
            // be removed, and the TLB cleaned, before a different memory type
            // is installed.
            (&raw mut (*table)[index]).write_volatile(0);
            asm!("dsb ishst", "tlbi vaae1is, {}", "dsb ish", in(reg) (index * BLOCK_SIZE) >> 12, options(nostack));
            (&raw mut (*table)[index]).write_volatile(new);
        }
    }
    // SAFETY: barriers only order memory accesses and instruction fetch.
    unsafe { asm!("dsb ishst", "isb", options(nostack)) };
    // The CPU may have speculatively re-read lines while the range was still
    // mapped cacheable; drop them now that it isn't.
    cache::clean_invalidate(start, len);
    true
}
