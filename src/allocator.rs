//! A first-fit free-list memory allocator.
//!
//! Free memory is kept as a list of blocks sorted by address, each block
//! storing its size and the next block's address in its own first 16 bytes.
//! Allocation takes the first block big enough (after alignment), splitting
//! off whatever is left before and after. Freeing puts the block back in
//! address order and merges it with free neighbours, so memory doesn't
//! fragment into ever smaller pieces.
//!
//! Every block's address and size are multiples of `MIN_BLOCK` (16 bytes),
//! so any leftover piece is either empty or big enough to hold its own
//! header.
//!
//! This file depends only on `core`, so it is also compiled into the host
//! unit tests in tools/heap-test.

use core::alloc::Layout;
use core::ptr::{self, NonNull};

/// Smallest block, and the granularity of all sizes and addresses.
pub const MIN_BLOCK: usize = 16;

#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

const _: () = assert!(core::mem::size_of::<FreeBlock>() <= MIN_BLOCK);

/// Heap usage figures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    pub total: usize,
    pub used: usize,
    pub free: usize,
    /// The largest single allocation that could currently succeed (ignoring
    /// alignment beyond `MIN_BLOCK`).
    pub largest_free: usize,
    pub free_blocks: usize,
}

pub struct Heap {
    /// First free block, lowest address first.
    head: *mut FreeBlock,
    start: usize,
    end: usize,
    used: usize,
}

// SAFETY: the heap only holds pointers into the memory it manages; whoever
// owns the `Heap` owns that memory.
unsafe impl Send for Heap {}

impl Heap {
    pub const fn empty() -> Self {
        Heap {
            head: ptr::null_mut(),
            start: 0,
            end: 0,
            used: 0,
        }
    }

    /// Hand the memory `[start, start + size)` to the heap. Its ends are
    /// trimmed to multiples of `MIN_BLOCK`.
    ///
    /// # Safety
    ///
    /// The memory must be valid, writable, unused by anything else for as
    /// long as the heap exists, and not overlap memory given before.
    pub unsafe fn init(&mut self, start: usize, size: usize) {
        let first = start.next_multiple_of(MIN_BLOCK);
        let end = (start + size) & !(MIN_BLOCK - 1);
        self.start = first;
        self.end = end.max(first);
        self.used = 0;
        self.head = ptr::null_mut();
        if end > first {
            let block = first as *mut FreeBlock;
            // SAFETY: the caller gave us this memory.
            unsafe {
                block.write(FreeBlock {
                    size: end - first,
                    next: ptr::null_mut(),
                })
            };
            self.head = block;
        }
    }

    /// The block size and alignment used for `layout`, or `None` if the size
    /// is too large to round up.
    fn block_layout(layout: Layout) -> Option<(usize, usize)> {
        let size = layout.size().max(1).checked_next_multiple_of(MIN_BLOCK)?;
        Some((size, layout.align().max(MIN_BLOCK)))
    }

    /// Allocate memory for `layout`, or return `None` if no free block is big
    /// enough.
    pub fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let (size, align) = Self::block_layout(layout)?;

        // `link` is the pointer that points at `block`, so the block can be
        // unlinked or replaced.
        let mut link: *mut *mut FreeBlock = &mut self.head;
        // SAFETY: every block on the list is free heap memory holding a
        // valid header, and we have exclusive access to the list.
        unsafe {
            while !(*link).is_null() {
                let block = *link;
                let start = block as usize;
                let end = start + (*block).size;

                // Both `start` and `align` are multiples of MIN_BLOCK, so the
                // gap before `alloc_start` is 0 or a whole block.
                let alloc_start = start.next_multiple_of(align);
                let alloc_end = alloc_start.checked_add(size);
                if let Some(alloc_end) = alloc_end.filter(|&e| e <= end) {
                    let next = (*block).next;
                    // What follows this block on the list after allocation:
                    // the piece left after the allocation, if any.
                    let after = if alloc_end < end {
                        let tail = alloc_end as *mut FreeBlock;
                        tail.write(FreeBlock {
                            size: end - alloc_end,
                            next,
                        });
                        tail
                    } else {
                        next
                    };
                    if alloc_start > start {
                        // Keep the piece before the allocation as a block.
                        (*block).size = alloc_start - start;
                        (*block).next = after;
                    } else {
                        *link = after;
                    }
                    self.used += size;
                    return NonNull::new(alloc_start as *mut u8);
                }
                link = &mut (*block).next;
            }
        }
        None
    }

    /// Return memory to the heap.
    ///
    /// # Safety
    ///
    /// `ptr` must have come from `allocate` on this heap with the same
    /// `layout`, and must not be used again.
    pub unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout) {
        let Some((size, _)) = Self::block_layout(layout) else {
            return;
        };
        let addr = ptr.as_ptr() as usize;
        debug_assert!(addr >= self.start && addr + size <= self.end);

        // SAFETY: as in `allocate`; the freed block is ours again.
        unsafe {
            // Find the free blocks either side of the freed one.
            let mut prev: *mut FreeBlock = ptr::null_mut();
            let mut next = self.head;
            while !next.is_null() && (next as usize) < addr {
                prev = next;
                next = (*next).next;
            }

            let block = addr as *mut FreeBlock;
            block.write(FreeBlock { size, next });
            // Merge with the following block if they touch.
            if !next.is_null() && addr + size == next as usize {
                (*block).size += (*next).size;
                (*block).next = (*next).next;
            }
            // Link in after the preceding block, merging if they touch.
            if prev.is_null() {
                self.head = block;
            } else if prev as usize + (*prev).size == addr {
                (*prev).size += (*block).size;
                (*prev).next = (*block).next;
            } else {
                (*prev).next = block;
            }
        }
        self.used -= size;
    }

    pub fn stats(&self) -> Stats {
        let mut free = 0;
        let mut largest_free = 0;
        let mut free_blocks = 0;
        let mut block = self.head;
        // SAFETY: walking the list of valid free blocks.
        unsafe {
            while !block.is_null() {
                free += (*block).size;
                largest_free = largest_free.max((*block).size);
                free_blocks += 1;
                block = (*block).next;
            }
        }
        Stats {
            total: self.end - self.start,
            used: self.used,
            free,
            largest_free,
            free_blocks,
        }
    }

    /// The memory the heap manages.
    pub fn region(&self) -> (usize, usize) {
        (self.start, self.end)
    }
}
