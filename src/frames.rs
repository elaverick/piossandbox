//! The physical frame allocator: hands out 4 KiB pages of RAM.
//!
//! Each region of usable RAM has a bitmap with one bit per frame (set =
//! in use). Allocation scans from where the last one left off. Freeing a
//! frame that isn't allocated, or isn't ours, panics: that is always a bug,
//! and carrying on would hand the same memory out twice.
//!
//! This file depends only on `core` (and `addr`), so the host unit tests
//! compile it too. The kernel's instance and `OwnedFrame` are in memory.rs.

use crate::addr::{PAGE_SIZE, PhysAddr};

/// A page-aligned physical address: the start of a frame.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Frame(PhysAddr);

impl Frame {
    /// The frame starting at `addr`, if it is page-aligned.
    pub const fn from_addr(addr: PhysAddr) -> Option<Frame> {
        if addr.is_page_aligned() {
            Some(Frame(addr))
        } else {
            None
        }
    }

    pub const fn addr(self) -> PhysAddr {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// No room for another region.
    TooManyRegions,
    /// The region start isn't page-aligned.
    Misaligned,
    /// The bitmap is too small for the region.
    BitmapTooSmall,
    /// The region overlaps one already added.
    Overlap,
}

const MAX_REGIONS: usize = 16;

struct Region<'a> {
    start: PhysAddr,
    frames: usize,
    /// One bit per frame, set when in use.
    bitmap: &'a mut [u64],
    /// Word to start the next search from.
    hint: usize,
}

impl Region<'_> {
    fn end(&self) -> PhysAddr {
        self.start + self.frames * PAGE_SIZE
    }

    fn contains(&self, addr: PhysAddr) -> bool {
        self.start <= addr && addr < self.end()
    }

    fn is_used(&self, index: usize) -> bool {
        self.bitmap[index / 64] & (1 << (index % 64)) != 0
    }

    fn set_used(&mut self, index: usize, used: bool) {
        let bit = 1u64 << (index % 64);
        if used {
            self.bitmap[index / 64] |= bit;
        } else {
            self.bitmap[index / 64] &= !bit;
        }
    }
}

pub struct FrameAllocator<'a> {
    regions: [Option<Region<'a>>; MAX_REGIONS],
    total: usize,
    free: usize,
}

impl<'a> FrameAllocator<'a> {
    pub const fn new() -> Self {
        FrameAllocator {
            regions: [const { None }; MAX_REGIONS],
            total: 0,
            free: 0,
        }
    }

    /// Bitmap words needed for a region of `frames` frames.
    pub const fn bitmap_words(frames: usize) -> usize {
        frames.div_ceil(64)
    }

    /// Manage `frames` frames from `start`, all free to begin with, tracked
    /// in `bitmap` (at least `bitmap_words(frames)` long).
    pub fn add_region(
        &mut self,
        start: PhysAddr,
        frames: usize,
        bitmap: &'a mut [u64],
    ) -> Result<(), FrameError> {
        if !start.is_page_aligned() {
            return Err(FrameError::Misaligned);
        }
        let words = Self::bitmap_words(frames);
        if bitmap.len() < words {
            return Err(FrameError::BitmapTooSmall);
        }
        let end = start + frames * PAGE_SIZE;
        if self
            .regions
            .iter()
            .flatten()
            .any(|r| r.start < end && start < r.end())
        {
            return Err(FrameError::Overlap);
        }
        let slot = self
            .regions
            .iter_mut()
            .find(|r| r.is_none())
            .ok_or(FrameError::TooManyRegions)?;
        let bitmap = &mut bitmap[..words];
        bitmap.fill(0);
        // Bits past the last frame are permanently "used".
        if !frames.is_multiple_of(64) {
            bitmap[words - 1] = !0 << (frames % 64);
        }
        *slot = Some(Region {
            start,
            frames,
            bitmap,
            hint: 0,
        });
        self.total += frames;
        self.free += frames;
        Ok(())
    }

    pub fn total_frames(&self) -> usize {
        self.total
    }

    pub fn free_frames(&self) -> usize {
        self.free
    }

    /// Is `addr` in RAM this allocator manages?
    pub fn contains(&self, addr: PhysAddr) -> bool {
        self.regions.iter().flatten().any(|r| r.contains(addr))
    }

    /// Allocate one frame, from the lowest region with a free one.
    pub fn allocate(&mut self) -> Option<Frame> {
        for region in self.regions.iter_mut().flatten() {
            let words = region.bitmap.len();
            for i in 0..words {
                let word = (region.hint + i) % words;
                let bits = region.bitmap[word];
                if bits != !0 {
                    let index = word * 64 + bits.trailing_ones() as usize;
                    region.set_used(index, true);
                    region.hint = word;
                    self.free -= 1;
                    return Frame::from_addr(region.start + index * PAGE_SIZE);
                }
            }
        }
        None
    }

    /// Allocate `count` physically contiguous frames starting at a multiple
    /// of `align` bytes (a power of two, at least a page).
    pub fn allocate_contiguous(&mut self, count: usize, align: usize) -> Option<PhysAddr> {
        if count == 0 || !align.is_power_of_two() || align < PAGE_SIZE {
            return None;
        }
        for region in self.regions.iter_mut().flatten() {
            let mut first = (region.start.align_up(align) - region.start) / PAGE_SIZE;
            while first + count <= region.frames {
                match (first..first + count).find(|&i| region.is_used(i)) {
                    None => {
                        for i in first..first + count {
                            region.set_used(i, true);
                        }
                        self.free -= count;
                        return Some(region.start + first * PAGE_SIZE);
                    }
                    // Skip past the used frame, keeping the alignment.
                    Some(used) => {
                        let next = region.start + (used + 1) * PAGE_SIZE;
                        first = (next.align_up(align) - region.start) / PAGE_SIZE;
                    }
                }
            }
        }
        None
    }

    /// Return a frame.
    ///
    /// # Panics
    ///
    /// If the frame isn't in a region or isn't allocated.
    pub fn free(&mut self, frame: Frame) {
        self.free_contiguous(frame.addr(), 1);
    }

    /// Return `count` contiguous frames starting at `start`.
    ///
    /// # Panics
    ///
    /// As `free`, for any of the frames.
    pub fn free_contiguous(&mut self, start: PhysAddr, count: usize) {
        assert!(
            start.is_page_aligned(),
            "freeing a misaligned frame {start:?}"
        );
        let region = self
            .regions
            .iter_mut()
            .flatten()
            .find(|r| r.contains(start))
            .unwrap_or_else(|| panic!("freeing frame {start:?}, which isn't managed memory"));
        let first = (start - region.start) / PAGE_SIZE;
        assert!(
            first + count <= region.frames,
            "freeing frames past the end of a region"
        );
        for i in first..first + count {
            assert!(
                region.is_used(i),
                "double free of frame {:?}",
                region.start + i * PAGE_SIZE
            );
            region.set_used(i, false);
        }
        self.free += count;
    }
}

impl Default for FrameAllocator<'_> {
    fn default() -> Self {
        Self::new()
    }
}
