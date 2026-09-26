//! A set of physical address ranges with fixed capacity, for working out
//! which memory is usable before there is a heap.
//!
//! This file depends only on `core` (and `addr`), so the host unit tests
//! compile it too.

use crate::addr::PhysAddr;

/// The set has no room for another range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Full;

/// Half-open ranges `[start, end)`, kept sorted, non-overlapping and
/// non-adjacent (touching ranges are merged).
#[derive(Clone, Copy)]
pub struct RangeSet<const N: usize> {
    ranges: [(PhysAddr, PhysAddr); N],
    len: usize,
}

impl<const N: usize> RangeSet<N> {
    pub const fn new() -> Self {
        RangeSet {
            ranges: [(PhysAddr::new(0), PhysAddr::new(0)); N],
            len: 0,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (PhysAddr, PhysAddr)> + '_ {
        self.ranges[..self.len].iter().copied()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Total bytes covered.
    pub fn total(&self) -> usize {
        self.iter().map(|(s, e)| e - s).sum()
    }

    pub fn contains(&self, addr: PhysAddr) -> bool {
        self.iter().any(|(s, e)| s <= addr && addr < e)
    }

    /// Add `[start, end)`, merging with anything it overlaps or touches.
    pub fn add(&mut self, start: PhysAddr, end: PhysAddr) -> Result<(), Full> {
        if start >= end {
            return Ok(());
        }
        let (mut start, mut end) = (start, end);
        // Absorb every range that overlaps or touches the new one.
        let mut i = 0;
        while i < self.len {
            let (s, e) = self.ranges[i];
            if s <= end && start <= e {
                start = start.min(s);
                end = end.max(e);
                self.remove_at(i);
            } else {
                i += 1;
            }
        }
        self.insert_sorted(start, end)
    }

    /// Remove `[start, end)`, splitting a range if needed.
    pub fn remove(&mut self, start: PhysAddr, end: PhysAddr) -> Result<(), Full> {
        if start >= end {
            return Ok(());
        }
        let mut i = 0;
        while i < self.len {
            let (s, e) = self.ranges[i];
            if e <= start || end <= s {
                i += 1;
                continue;
            }
            self.remove_at(i);
            // Put back whatever lies outside the removed range.
            if s < start {
                self.insert_sorted(s, start)?;
                i += 1;
            }
            if end < e {
                self.insert_sorted(end, e)?;
                i += 1;
            }
        }
        Ok(())
    }

    /// Shrink every range to `align`-aligned bounds, dropping any that
    /// become empty.
    pub fn align_inward(&mut self, align: usize) {
        let mut i = 0;
        while i < self.len {
            let (s, e) = self.ranges[i];
            let (s, e) = (s.align_up(align), e.align_down(align));
            if s < e {
                self.ranges[i] = (s, e);
                i += 1;
            } else {
                self.remove_at(i);
            }
        }
    }

    fn remove_at(&mut self, i: usize) {
        self.ranges.copy_within(i + 1..self.len, i);
        self.len -= 1;
    }

    fn insert_sorted(&mut self, start: PhysAddr, end: PhysAddr) -> Result<(), Full> {
        if self.len == N {
            return Err(Full);
        }
        let at = self.ranges[..self.len]
            .iter()
            .position(|&(s, _)| s > start)
            .unwrap_or(self.len);
        self.ranges.copy_within(at..self.len, at + 1);
        self.ranges[at] = (start, end);
        self.len += 1;
        Ok(())
    }
}

impl<const N: usize> Default for RangeSet<N> {
    fn default() -> Self {
        Self::new()
    }
}
