//! Tests for src/ranges.rs.

use crate::addr::PhysAddr;
use crate::ranges::{Full, RangeSet};

fn p(a: usize) -> PhysAddr {
    PhysAddr::new(a)
}

fn list<const N: usize>(set: &RangeSet<N>) -> Vec<(usize, usize)> {
    set.iter().map(|(s, e)| (s.as_usize(), e.as_usize())).collect()
}

#[test]
fn add_merges_overlapping_and_touching() {
    let mut s = RangeSet::<8>::new();
    s.add(p(100), p(200)).unwrap();
    s.add(p(300), p(400)).unwrap();
    assert_eq!(list(&s), [(100, 200), (300, 400)]);
    s.add(p(200), p(300)).unwrap(); // touches both
    assert_eq!(list(&s), [(100, 400)]);
    s.add(p(50), p(120)).unwrap();
    s.add(p(0), p(10)).unwrap();
    assert_eq!(list(&s), [(0, 10), (50, 400)]);
    assert_eq!(s.total(), 360);
}

#[test]
fn remove_trims_and_splits() {
    let mut s = RangeSet::<8>::new();
    s.add(p(0), p(1000)).unwrap();
    s.remove(p(100), p(200)).unwrap();
    assert_eq!(list(&s), [(0, 100), (200, 1000)]);
    s.remove(p(0), p(50)).unwrap();
    s.remove(p(900), p(2000)).unwrap();
    assert_eq!(list(&s), [(50, 100), (200, 900)]);
    s.remove(p(60), p(800)).unwrap(); // spans a gap
    assert_eq!(list(&s), [(50, 60), (800, 900)]);
    s.remove(p(0), p(5000)).unwrap();
    assert!(s.is_empty());
}

#[test]
fn align_inward_drops_slivers() {
    let mut s = RangeSet::<8>::new();
    s.add(p(0x1001), p(0x5fff)).unwrap();
    s.add(p(0x8100), p(0x8200)).unwrap();
    s.align_inward(0x1000);
    assert_eq!(list(&s), [(0x2000, 0x5000)]);
}

#[test]
fn reports_when_full() {
    let mut s = RangeSet::<2>::new();
    s.add(p(0), p(10)).unwrap();
    s.add(p(20), p(30)).unwrap();
    assert_eq!(s.add(p(40), p(50)), Err(Full));
    // Splitting needs a slot too.
    assert_eq!(s.remove(p(2), p(4)), Err(Full));
}

#[test]
fn matches_a_byte_model() {
    let mut rng = 0x1234_5678u64;
    let mut next = |n: usize| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % n as u64) as usize
    };
    let mut set = RangeSet::<64>::new();
    let mut model = vec![false; 512];
    for _ in 0..5000 {
        let (a, b) = (next(512), next(512));
        let (start, end) = (a.min(b), a.max(b));
        if next(2) == 0 {
            if set.add(p(start), p(end)).is_ok() {
                model[start..end].fill(true);
            }
        } else if set.remove(p(start), p(end)).is_ok() {
            model[start..end].fill(false);
        }
        let ranges = list(&set);
        for w in ranges.windows(2) {
            assert!(w[0].1 < w[1].0, "sorted, disjoint, not touching: {ranges:?}");
        }
        for (i, &inside) in model.iter().enumerate() {
            assert_eq!(set.contains(p(i)), inside, "byte {i}");
        }
    }
}
