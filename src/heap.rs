//! The kernel heap: backs `alloc` (`Box`, `Vec`, `String`, ...).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;

use crate::addr::{PAGE_SIZE, VirtAddr};
use crate::allocator::{Heap, Stats};
use crate::memory;
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

/// Heap size: a sixteenth of usable RAM, between these limits.
const MIN_SIZE: usize = 4 << 20;
const MAX_SIZE: usize = 64 << 20;

/// Give the heap its memory: contiguous frames from the frame allocator.
/// Returns where the heap is, or `None` if there wasn't enough memory.
pub fn init() -> Option<(VirtAddr, usize)> {
    let usable = memory::frame_stats().free * PAGE_SIZE;
    let size = (usable / 16).clamp(MIN_SIZE, MAX_SIZE);
    let start = memory::allocate_permanent(size)?.to_virt();
    let mut heap = ALLOCATOR.0.lock();
    // SAFETY: these frames were just allocated for the heap, for good; they
    // are RAM, mapped read-write in the linear map, and nothing else uses
    // them.
    unsafe { heap.init(start.as_usize(), size) };
    Some((start, size))
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
