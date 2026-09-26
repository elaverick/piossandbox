//! Memory the controller reads and writes: one block from the kernel,
//! uncached, handed out a page at a time.

use libpios::{Error, Handle};

pub const PAGE_SIZE: usize = 4096;

/// A page of DMA memory: where we see it, and where the controller does.
#[derive(Clone, Copy)]
pub struct Page {
    pub va: usize,
    pub bus: u64,
}

impl Page {
    pub fn read32(&self, offset: usize) -> u32 {
        assert!(offset + 4 <= PAGE_SIZE);
        // SAFETY: the page is ours, mapped for good, and the offset is
        // within it and aligned by every caller.
        unsafe { ((self.va + offset) as *const u32).read_volatile() }
    }

    pub fn write32(&self, offset: usize, value: u32) {
        assert!(offset + 4 <= PAGE_SIZE);
        // SAFETY: as for `read32`.
        unsafe { ((self.va + offset) as *mut u32).write_volatile(value) }
    }

    pub fn write64(&self, offset: usize, value: u64) {
        self.write32(offset, value as u32);
        self.write32(offset + 4, (value >> 32) as u32);
    }

    pub fn read8(&self, offset: usize) -> u8 {
        assert!(offset < PAGE_SIZE);
        // SAFETY: as for `read32`.
        unsafe { ((self.va + offset) as *const u8).read_volatile() }
    }

    pub fn zero(&self) {
        for offset in (0..PAGE_SIZE).step_by(4) {
            self.write32(offset, 0);
        }
    }
}

pub struct DmaPool {
    first: Page,
    pages: usize,
    used: usize,
}

impl DmaPool {
    /// Get `pages` pages from the kernel, mapped at `va`.
    pub fn new(dma: &Handle, va: usize, pages: usize) -> Result<DmaPool, Error> {
        let bus = dma.dma_alloc(va, pages)?;
        Ok(DmaPool {
            first: Page { va, bus },
            pages,
            used: 0,
        })
    }

    /// A fresh, zeroed page (the kernel zeroes the block; pages are never
    /// given back, only reused by their owner).
    pub fn page(&mut self) -> Option<Page> {
        if self.used == self.pages {
            return None;
        }
        let offset = self.used * PAGE_SIZE;
        self.used += 1;
        Some(Page {
            va: self.first.va + offset,
            bus: self.first.bus + offset as u64,
        })
    }
}

/// Make our writes to DMA memory visible to the controller before telling
/// it about them (the memory is uncached, but writes can still be
/// buffered).
pub fn write_barrier() {
    // SAFETY: a barrier only orders memory accesses.
    unsafe { core::arch::asm!("dsb st", options(nostack, preserves_flags)) };
}

/// Make sure reads of DMA memory after this see what the controller wrote
/// before what we read before it.
pub fn read_barrier() {
    // SAFETY: a barrier only orders memory accesses.
    unsafe { core::arch::asm!("dsb ld", options(nostack, preserves_flags)) };
}
