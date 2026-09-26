//! The kernel's memory map and MMU control.
//!
//! boot.s turns the MMU on with a small boot map (the first GiB of RAM and
//! the peripherals, used both as an identity map and for the kernel half).
//! `install_kernel_map` then builds the real map for the kernel half
//! (TTBR1) and removes the identity map. The lower half (TTBR0) belongs to
//! whichever user address space is active, or to an empty table.
//!
//! The kernel half is a linear map: physical `p` is at `KERNEL_BASE + p`.
//!
//! | What | Mapped as |
//! | --- | --- |
//! | kernel code | read-execute |
//! | kernel constants | read-only |
//! | kernel data, boot stack, all other RAM | read-write, never executable |
//! | thread stacks, above the linear map | read-write, never executable |
//! | peripherals | Device, read-write, never executable |
//! | framebuffer | Normal non-cacheable, read-write, never executable |
//!
//! Everything else is unmapped, so a stray access faults. That includes a
//! guard page below every kernel stack.

use core::arch::asm;

use crate::addr::{KERNEL_BASE, LINEAR_MAP_SIZE, PAGE_SIZE, PhysAddr, VirtAddr};
use crate::memory;
use crate::paging::{Access, Attributes, ENTRIES, Half, MapError, PageTable, TableMemory};
use crate::ranges::RangeSet;
use crate::sync::SpinLock;

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

/// Page tables for kernel use, in frames from the frame allocator, reached
/// through the linear map.
pub struct KernelTableMemory;

// SAFETY: tables are whole frames from the frame allocator, owned by the
// page table until freed, and the linear map covers all RAM.
unsafe impl TableMemory for KernelTableMemory {
    fn allocate_table(&mut self) -> Option<PhysAddr> {
        memory::allocate_zeroed_frame()
    }

    unsafe fn free_table(&mut self, table: PhysAddr) {
        // SAFETY: the caller guarantees nothing references the table.
        unsafe { memory::free_frame(table) };
    }

    fn table(&self, table: PhysAddr) -> *mut [u64; ENTRIES] {
        table.to_virt().as_ptr()
    }
}

/// The kernel half's page tables, once built.
static KERNEL_MAP: SpinLock<Option<PageTable<KernelTableMemory>>> = SpinLock::new(None);

/// The lower half's table while no user address space is active: maps
/// nothing, so any access below the kernel half faults.
#[repr(C, align(4096))]
struct EmptyTable([u64; ENTRIES]);
static EMPTY_TABLE: EmptyTable = EmptyTable([0; ENTRIES]);

unsafe extern "C" {
    static __kernel_start: u8;
    static __text_end: u8;
    static __rodata_end: u8;
    static __stack_guard: u8;
    static __stack_bottom: u8;
    static __kernel_end: u8;
    /// In boot.s: switch TTBR1 to `root`, from the identity map.
    fn switch_kernel_tables(root: usize);
}

/// The physical range of the kernel image, from its first byte to the end
/// of its stack.
pub fn kernel_image() -> (PhysAddr, PhysAddr) {
    let phys = |symbol: *const u8| {
        VirtAddr::from_ptr(symbol)
            .to_phys()
            .expect("kernel is in the linear map")
    };
    (
        phys(&raw const __kernel_start),
        phys(&raw const __kernel_end),
    )
}

/// What goes in the kernel map, besides the kernel itself.
pub struct KernelMapPlan<'a> {
    /// All RAM to map (read-write, never executable).
    pub ram: &'a RangeSet<32>,
    /// Peripheral ranges.
    pub devices: &'a [(PhysAddr, usize)],
    /// The framebuffer, if any.
    pub framebuffer: Option<(PhysAddr, usize)>,
}

/// Build the kernel map, switch to it and remove the identity map.
pub fn install_kernel_map(plan: &KernelMapPlan) -> Result<(), MapError> {
    let mut map = PageTable::new(Half::Upper, KernelTableMemory)?;
    let at = |p: PhysAddr| p.to_virt().as_usize();

    // The kernel image, section by section. (linker.ld page-aligns them.)
    let phys = |symbol: *const u8| {
        VirtAddr::from_ptr(symbol)
            .to_phys()
            .expect("kernel is in the linear map")
    };
    let text = phys(&raw const __kernel_start);
    let rodata = phys(&raw const __text_end);
    let data = phys(&raw const __rodata_end);
    let guard = phys(&raw const __stack_guard);
    let stack = phys(&raw const __stack_bottom);
    let end = phys(&raw const __kernel_end);
    // (The boot stack's guard page, between `guard` and `stack`, stays
    // unmapped.)
    let sections = [
        (text, rodata, Access::KERNEL_READ_EXECUTE),
        (rodata, data, Access::KERNEL_READ),
        (data, guard, Access::KERNEL_READ_WRITE),
        (stack, end, Access::KERNEL_READ_WRITE),
    ];
    for (start, end, access) in sections {
        map.map(
            at(start),
            start,
            end - start,
            Attributes::normal(access),
            false,
        )?;
    }

    // All other RAM.
    let mut ram = *plan.ram;
    ram.remove(text, end).map_err(|_| MapError::OutOfMemory)?;
    for (start, end) in ram.iter() {
        map.map(
            at(start),
            start,
            end - start,
            Attributes::normal(Access::KERNEL_READ_WRITE),
            true,
        )?;
    }

    for &(start, size) in plan.devices {
        map.map(
            at(start),
            start,
            size,
            Attributes::device(Access::KERNEL_READ_WRITE),
            true,
        )?;
    }
    if let Some((start, size)) = plan.framebuffer {
        let size = size.next_multiple_of(PAGE_SIZE);
        map.map(
            at(start),
            start,
            size,
            Attributes::non_cacheable(Access::KERNEL_READ_WRITE),
            true,
        )?;
    }

    // Make the new tables visible to the table walker, then switch.
    // SAFETY: a barrier only orders memory accesses.
    unsafe { asm!("dsb ishst", options(nostack)) };
    let root = map.root().as_usize();
    crate::irq::without_interrupts(|| {
        // SAFETY: the new map covers everything the kernel uses (its image,
        // stack, heap-to-be, the device tree's RAM and the devices), with
        // the same addresses as the boot map.
        unsafe { switch_kernel_tables(root) };
    });
    set_user_tables(None);
    *KERNEL_MAP.lock() = Some(map);
    Ok(())
}

/// Map `frames`, one page each, read-write and never executable at `va` in
/// the kernel half, above the linear map. On error nothing is mapped.
pub fn map_kernel_pages(va: usize, frames: &[PhysAddr]) -> Result<(), MapError> {
    assert!(
        va >= KERNEL_BASE + LINEAR_MAP_SIZE,
        "not above the linear map"
    );
    let mut map = KERNEL_MAP.lock();
    let map = map.as_mut().expect("the kernel map is installed");
    for (i, &frame) in frames.iter().enumerate() {
        let attributes = Attributes::normal(Access::KERNEL_READ_WRITE);
        if let Err(e) = map.map(va + i * PAGE_SIZE, frame, PAGE_SIZE, attributes, false) {
            if i > 0 {
                map.unmap(va, i * PAGE_SIZE)
                    .expect("the pages just mapped are there");
                flush_kernel_tlb(va, i);
            }
            return Err(e);
        }
    }
    // Make the new entries visible to the table walker. (Unmapped entries
    // are never cached in the TLB, so there is nothing to invalidate.)
    // SAFETY: barriers only order memory accesses.
    unsafe { asm!("dsb ishst", "isb", options(nostack)) };
    Ok(())
}

/// Unmap `pages` pages at `va`, mapped by `map_kernel_pages`.
pub fn unmap_kernel_pages(va: usize, pages: usize) -> Result<(), MapError> {
    assert!(
        va >= KERNEL_BASE + LINEAR_MAP_SIZE,
        "not above the linear map"
    );
    let mut map = KERNEL_MAP.lock();
    let map = map.as_mut().expect("the kernel map is installed");
    map.unmap(va, pages * PAGE_SIZE)?;
    flush_kernel_tlb(va, pages);
    Ok(())
}

/// Discard any cached translations for `pages` pages at `va`.
fn flush_kernel_tlb(va: usize, pages: usize) {
    // SAFETY: TLB invalidation only discards cached translations.
    unsafe { asm!("dsb ishst", options(nostack)) };
    for i in 0..pages {
        // The operand is VA[55:12]; the bits above are other fields.
        let operand = ((va + i * PAGE_SIZE) >> 12) & ((1 << 44) - 1);
        // SAFETY: as above.
        unsafe { asm!("tlbi vaae1, {}", in(reg) operand, options(nostack)) };
    }
    // SAFETY: barriers only order memory accesses.
    unsafe { asm!("dsb ish", "isb", options(nostack)) };
}

/// How the kernel map translates `va`.
pub fn kernel_translate(va: VirtAddr) -> Option<(PhysAddr, Attributes)> {
    KERNEL_MAP.lock().as_ref()?.translate(va.as_usize())
}

/// The lower half's table while no address space is active.
pub fn user_tables_when_idle() -> PhysAddr {
    VirtAddr::from_ptr(&EMPTY_TABLE)
        .to_phys()
        .expect("in the linear map")
}

/// Point the lower half at `root` (or at nothing), flushing stale
/// translations. (There are no address space IDs yet, so every switch
/// flushes the whole TLB.)
pub fn set_user_tables(root: Option<PhysAddr>) {
    let root = root.unwrap_or_else(user_tables_when_idle);
    // SAFETY: `root` is a valid top-level table (an address space's, or
    // the empty one); the lower half holds no kernel code or data.
    unsafe {
        asm!("dsb ishst", "msr ttbr0_el1, {}", "isb", "tlbi vmalle1", "dsb ish", "isb",
             in(reg) root.as_usize(), options(nostack));
    }
}

/// The lower half's current top-level table.
pub fn user_tables() -> PhysAddr {
    let ttbr0: u64;
    // SAFETY: reading TTBR0_EL1 has no side effects.
    unsafe { asm!("mrs {}, ttbr0_el1", out(reg) ttbr0, options(nomem, nostack)) };
    PhysAddr::new((ttbr0 & 0x0000_FFFF_FFFF_FFFE) as usize)
}

/// Flush all translations for the lower half (after unmapping).
pub fn flush_user_tlb() {
    // SAFETY: TLB invalidation only discards cached translations.
    unsafe {
        asm!(
            "dsb ishst",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            options(nostack)
        )
    };
}

/// A kind of access to check with `probe`.
#[derive(Clone, Copy)]
pub enum Probe {
    KernelRead,
    KernelWrite,
    UserRead,
    UserWrite,
}

/// Ask the MMU whether an access to `va` would succeed, without making it:
/// the physical address, or the fault status code (as in an abort's ESR).
pub fn probe(va: usize, kind: Probe) -> Result<PhysAddr, u8> {
    let par: u64;
    // SAFETY: address translation instructions only update PAR_EL1.
    unsafe {
        match kind {
            Probe::KernelRead => {
                asm!("at s1e1r, {}", "isb", "mrs {}, par_el1", in(reg) va, out(reg) par, options(nostack))
            }
            Probe::KernelWrite => {
                asm!("at s1e1w, {}", "isb", "mrs {}, par_el1", in(reg) va, out(reg) par, options(nostack))
            }
            Probe::UserRead => {
                asm!("at s1e0r, {}", "isb", "mrs {}, par_el1", in(reg) va, out(reg) par, options(nostack))
            }
            Probe::UserWrite => {
                asm!("at s1e0w, {}", "isb", "mrs {}, par_el1", in(reg) va, out(reg) par, options(nostack))
            }
        }
    }
    if par & 1 != 0 {
        Err(((par >> 1) & 0x3F) as u8)
    } else {
        Ok(PhysAddr::new(
            (par & 0x0000_FFFF_FFFF_F000) as usize | (va & (PAGE_SIZE - 1)),
        ))
    }
}

/// Fault status codes from `probe`, by kind (bits [5:2]).
pub fn is_translation_fault(status: u8) -> bool {
    status & 0x3C == 0x04
}

pub fn is_permission_fault(status: u8) -> bool {
    status & 0x3C == 0x0C
}
