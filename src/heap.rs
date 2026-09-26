//! The kernel heap: backs `alloc` (`Box`, `Vec`, `String`, ...).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;

use crate::allocator::{Heap, Stats};
use crate::fdt::Fdt;
use crate::mmu;
use crate::sync::SpinLock;

struct KernelAllocator(SpinLock<Heap>);

// SAFETY: `Heap` hands out non-overlapping blocks of at least the requested
// size and alignment, and the lock serialises access to it.
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0
            .lock()
            .allocate(layout)
            .map_or(core::ptr::null_mut(), NonNull::as_ptr)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller passes a pointer from `alloc` with its layout.
        unsafe {
            self.0
                .lock()
                .deallocate(NonNull::new_unchecked(ptr), layout)
        }
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator(SpinLock::new(Heap::empty()));

unsafe extern "C" {
    static __kernel_end: u8;
}

/// How much to use if the firmware can't tell us how much RAM there is.
const FALLBACK_SIZE: usize = 16 << 20;
const PAGE: usize = 4096;

/// Give the heap the free RAM after the kernel, up to the end of the ARM's
/// share of RAM (`ram_end`, from the firmware), avoiding the device tree and
/// anything it lists as reserved: the largest gap between those is used.
/// Returns the heap's `(start, end)`, or `None` if no usable memory was
/// found.
pub fn init(ram_end: Option<usize>, fdt: Option<&Fdt>) -> Option<(usize, usize)> {
    let low = (&raw const __kernel_end as usize).next_multiple_of(PAGE);
    let mut high = ram_end
        .unwrap_or(low + FALLBACK_SIZE)
        .min(mmu::MAPPED_RAM_END);

    // Ranges to keep out of, collected without a heap (we're making it).
    const MAX_HOLES: usize = 16;
    let mut holes = [(0usize, 0usize); MAX_HOLES];
    let mut count = 0;
    let mut avoid = |from: usize, size: usize| {
        let to = from.saturating_add(size);
        if count < MAX_HOLES {
            holes[count] = (from, to);
            count += 1;
        } else if to > low {
            // No room to track it: just stop the heap before it.
            high = high.min(from.max(low));
        }
    };
    if let Some(fdt) = fdt {
        avoid(fdt.addr(), fdt.size());
        for (addr, size) in fdt.reservations() {
            avoid(addr, size);
        }
    }
    let holes = &mut holes[..count];
    holes.sort_unstable();

    // Walk the gaps between the holes and keep the biggest.
    let mut best = (0, 0);
    let mut cursor = low;
    for &(from, to) in holes.iter().chain(core::iter::once(&(high, high))) {
        let gap_end = from.min(high) & !(PAGE - 1);
        let gap_start = cursor.next_multiple_of(PAGE);
        if gap_end > gap_start && gap_end - gap_start > best.1 - best.0 {
            best = (gap_start, gap_end);
        }
        cursor = cursor.max(to);
    }
    let (start, end) = best;
    if end <= start {
        return None;
    }

    let mut heap = ALLOCATOR.0.lock();
    // SAFETY: this RAM is mapped, lies after everything the kernel image and
    // stack use, and avoids the device tree and reserved ranges; nothing
    // else uses it.
    unsafe { heap.init(start, end - start) };
    Some(heap.region())
}

pub fn stats() -> Stats {
    ALLOCATOR.0.lock().stats()
}

/// Exercise the heap through the standard collections, then with random
/// allocations whose contents are checked, and make sure everything comes
/// back.
pub fn self_test() -> Result<(), &'static str> {
    let before = stats();
    if before.total == 0 {
        return Err("there is no heap");
    }
    {
        let boxed = Box::new(0x1234_5678_9ABC_DEF0u64);
        let mut numbers: Vec<u32> = Vec::new();
        for i in 0..10_000 {
            numbers.push(i); // grows by reallocating many times
        }
        let text = alloc::format!("{} {}", "heap", *boxed >> 32);
        let mut map = BTreeMap::new();
        for (i, word) in ["zero", "one", "two", "three"].iter().enumerate() {
            map.insert(*word, i);
        }
        if *boxed != 0x1234_5678_9ABC_DEF0
            || numbers.iter().map(|&n| n as u64).sum::<u64>() != 49_995_000
            || text != "heap 305419896"
            || map.get("two") != Some(&2)
        {
            return Err("collections gave wrong results");
        }

        let page = Layout::from_size_align(100, 4096).unwrap();
        // SAFETY: non-zero size; freed below with the same layout.
        let p = unsafe { alloc::alloc::alloc(page) };
        if p.is_null() || !(p as usize).is_multiple_of(4096) {
            return Err("page-aligned allocation failed");
        }
        // SAFETY: allocated above.
        unsafe { alloc::alloc::dealloc(p, page) };

        stress()?;
    }
    let after = stats();
    if after.used != before.used || after.largest_free != before.largest_free {
        return Err("memory was not all returned");
    }
    Ok(())
}

/// Random allocations and frees, each block filled with its own byte and
/// checked when freed, so any overlap shows up.
fn stress() -> Result<(), &'static str> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut random = move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    };
    let mut live: Vec<(Box<[u8]>, u8)> = Vec::with_capacity(64);
    for step in 0..4000 {
        if live.len() < 64 && (live.is_empty() || random(100) < 55) {
            let fill = step as u8;
            let size = 1 + random(2048);
            let mut block = alloc::vec![0u8; size].into_boxed_slice();
            block.fill(fill);
            live.push((block, fill));
        } else {
            let (block, fill) = live.swap_remove(random(live.len()));
            if block.iter().any(|&b| b != fill) {
                return Err("heap blocks overlapped");
            }
        }
    }
    Ok(())
}
