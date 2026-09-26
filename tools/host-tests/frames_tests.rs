//! Tests for src/frames.rs.

use std::collections::HashSet;

use crate::addr::{PAGE_SIZE, PhysAddr};
use crate::frames::{Frame, FrameAllocator, FrameError};

fn bitmap(frames: usize) -> &'static mut [u64] {
    Box::leak(vec![0u64; FrameAllocator::bitmap_words(frames)].into_boxed_slice())
}

fn p(a: usize) -> PhysAddr {
    PhysAddr::new(a)
}

#[test]
fn frames_must_be_page_aligned() {
    assert!(Frame::from_addr(p(0x1000)).is_some());
    assert!(Frame::from_addr(p(0x1004)).is_none());
}

#[test]
fn allocates_every_frame_once_then_runs_out() {
    let mut a = FrameAllocator::new();
    a.add_region(p(0x10_0000), 70, bitmap(70)).unwrap(); // not a multiple of 64
    let mut seen = HashSet::new();
    while let Some(f) = a.allocate() {
        let addr = f.addr().as_usize();
        assert!((0x10_0000..0x10_0000 + 70 * PAGE_SIZE).contains(&addr));
        assert!(seen.insert(addr), "handed out twice");
    }
    assert_eq!(seen.len(), 70);
    assert_eq!(a.free_frames(), 0);
}

#[test]
fn regions_are_used_lowest_first_and_checked() {
    let mut a = FrameAllocator::new();
    a.add_region(p(0x4000_0000), 10, bitmap(10)).unwrap();
    a.add_region(p(0x1000_0000), 10, bitmap(10)).unwrap();
    assert_eq!(a.add_region(p(0x4000_1000), 1, bitmap(1)), Err(FrameError::Overlap));
    assert_eq!(a.add_region(p(0x123), 1, bitmap(1)), Err(FrameError::Misaligned));
    assert_eq!(a.add_region(p(0x9000_0000), 100, bitmap(1)), Err(FrameError::BitmapTooSmall));
    assert_eq!(a.total_frames(), 20);
    assert!(a.contains(p(0x1000_9fff)));
    assert!(!a.contains(p(0x1000_a000)));
}

#[test]
fn contiguous_allocations_are_aligned_and_skip_used_frames() {
    let mut a = FrameAllocator::new();
    a.add_region(p(0x1000), 1000, bitmap(1000)).unwrap(); // starts off 64 KiB alignment
    let single = a.allocate().unwrap();
    let run = a.allocate_contiguous(16, 0x10000).unwrap();
    assert_eq!(run.as_usize() % 0x10000, 0);
    assert!(run > single.addr());
    let run2 = a.allocate_contiguous(16, 0x10000).unwrap();
    assert_eq!(run2 - run, 0x10000);
    assert_eq!(a.free_frames(), 1000 - 1 - 32);
    a.free_contiguous(run, 16);
    assert_eq!(a.allocate_contiguous(16, 0x10000), Some(run), "freed run is reused");
    assert_eq!(a.allocate_contiguous(2000, PAGE_SIZE), None);
    assert_eq!(a.allocate_contiguous(1, 3000), None, "alignment must be a power of two >= a page");
}

#[test]
fn contiguous_allocations_respect_a_limit() {
    let mut a = FrameAllocator::new();
    a.add_region(p(0x10_0000), 16, bitmap(16)).unwrap(); // 1 MiB .. 1 MiB + 64 KiB
    a.add_region(p(0x100_0000), 16, bitmap(16)).unwrap(); // 16 MiB .. + 64 KiB
    let limit = p(0x10_0000 + 8 * PAGE_SIZE);
    assert_eq!(a.allocate_contiguous_below(9, PAGE_SIZE, limit), None, "would end past the limit");
    let run = a.allocate_contiguous_below(8, PAGE_SIZE, limit).unwrap();
    assert_eq!(run, p(0x10_0000));
    assert_eq!(a.allocate_contiguous_below(1, PAGE_SIZE, limit), None, "nothing left below it");
    assert_eq!(a.allocate_contiguous_below(1, PAGE_SIZE, p(0x10_0000)), None);
    // Without a limit, the rest of the first region is found first.
    assert_eq!(a.allocate_contiguous(8, PAGE_SIZE), Some(p(0x10_0000 + 8 * PAGE_SIZE)));
    assert_eq!(a.allocate_contiguous_below(16, PAGE_SIZE, p(usize::MAX)), Some(p(0x100_0000)));
}

#[test]
#[should_panic(expected = "double free")]
fn double_free_panics() {
    let mut a = FrameAllocator::new();
    a.add_region(p(0), 8, bitmap(8)).unwrap();
    let f = a.allocate().unwrap();
    a.free(f);
    a.free(f);
}

#[test]
#[should_panic(expected = "isn't managed memory")]
fn freeing_foreign_memory_panics() {
    let mut a = FrameAllocator::new();
    a.add_region(p(0), 8, bitmap(8)).unwrap();
    a.free(Frame::from_addr(p(0x100_0000)).unwrap());
}

#[test]
fn randomized_against_a_model() {
    let mut a = FrameAllocator::new();
    a.add_region(p(0x0), 300, bitmap(300)).unwrap();
    a.add_region(p(0x800_0000), 129, bitmap(129)).unwrap();
    let mut live: Vec<(PhysAddr, usize)> = Vec::new();
    let mut used = HashSet::new();
    let mut rng = 0xdead_beefu64;
    let mut next = |n: usize| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % n as u64) as usize
    };
    for _ in 0..20_000 {
        if live.is_empty() || next(100) < 55 {
            let (start, count) = if next(4) == 0 {
                let count = 1 + next(12);
                match a.allocate_contiguous(count, PAGE_SIZE << next(4)) {
                    Some(s) => (s, count),
                    None => continue,
                }
            } else {
                match a.allocate() {
                    Some(f) => (f.addr(), 1),
                    None => continue,
                }
            };
            for i in 0..count {
                assert!(used.insert(start.as_usize() + i * PAGE_SIZE), "overlapping allocation");
            }
            live.push((start, count));
        } else {
            let (start, count) = live.swap_remove(next(live.len()));
            a.free_contiguous(start, count);
            for i in 0..count {
                used.remove(&(start.as_usize() + i * PAGE_SIZE));
            }
        }
        assert_eq!(a.free_frames() + used.len(), a.total_frames());
    }
}
