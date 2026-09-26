//! Tests for src/paging.rs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::addr::{PAGE_SIZE, PhysAddr};
use crate::paging::*;

const GIB: usize = 1 << 30;
const MIB2: usize = 2 << 20;

/// Page tables in host memory, at made-up physical addresses.
#[derive(Default)]
struct Tables {
    tables: HashMap<usize, Box<[u64; ENTRIES]>>,
    next: usize,
    /// Refuse to allocate once this many tables exist.
    limit: Option<usize>,
    allocations: usize,
}

#[derive(Clone, Default)]
struct FakeMemory(Rc<RefCell<Tables>>);

impl FakeMemory {
    fn live(&self) -> usize {
        self.0.borrow().tables.len()
    }
    fn limit(&self, n: usize) {
        self.0.borrow_mut().limit = Some(n);
    }
}

// SAFETY: tables are boxed, so their addresses stay put until freed.
unsafe impl TableMemory for FakeMemory {
    fn allocate_table(&mut self) -> Option<PhysAddr> {
        let mut t = self.0.borrow_mut();
        if t.limit.is_some_and(|l| t.tables.len() >= l) {
            return None;
        }
        t.next += PAGE_SIZE;
        t.allocations += 1;
        let addr = 0x4000_0000 + t.next;
        t.tables.insert(addr, Box::new([0; ENTRIES]));
        Some(PhysAddr::new(addr))
    }
    unsafe fn free_table(&mut self, table: PhysAddr) {
        assert!(self.0.borrow_mut().tables.remove(&table.as_usize()).is_some(), "freed an unknown table");
    }
    fn table(&self, table: PhysAddr) -> *mut [u64; ENTRIES] {
        let mut t = self.0.borrow_mut();
        let b = t.tables.get_mut(&table.as_usize()).expect("unknown table");
        &mut **b as *mut [u64; ENTRIES]
    }
}

fn lower() -> (PageTable<FakeMemory>, FakeMemory) {
    let m = FakeMemory::default();
    (PageTable::new(Half::Lower, m.clone()).unwrap(), m)
}

fn p(a: usize) -> PhysAddr {
    PhysAddr::new(a)
}

const RW: Attributes = Attributes::normal(Access::USER_READ_WRITE);

#[test]
fn no_access_is_both_writable_and_executable() {
    for a in [
        Access::KERNEL_READ,
        Access::KERNEL_READ_WRITE,
        Access::KERNEL_READ_EXECUTE,
        Access::USER_READ,
        Access::USER_READ_WRITE,
        Access::USER_READ_EXECUTE,
    ] {
        assert!(!(a.is_writable() && a.is_executable()), "{a:?}");
    }
}

#[test]
fn device_and_non_cacheable_memory_is_never_executable() {
    assert!(!Attributes::device(Access::KERNEL_READ_EXECUTE).access().is_executable());
    assert!(!Attributes::non_cacheable(Access::USER_READ_EXECUTE).access().is_executable());
    assert!(Attributes::normal(Access::KERNEL_READ_EXECUTE).access().is_executable());
}

#[test]
fn maps_and_translates_pages() {
    let (mut t, _) = lower();
    t.map(0x40_0000, p(0x1234_5000), 3 * PAGE_SIZE, RW, false).unwrap();
    assert_eq!(t.translate(0x40_0000), Some((p(0x1234_5000), RW)));
    assert_eq!(t.translate(0x40_2abc).unwrap().0, p(0x1234_7abc));
    assert_eq!(t.translate(0x40_3000), None);
    assert_eq!(t.translate(0x3f_f000), None);
}

#[test]
fn every_access_round_trips_through_the_descriptor() {
    let (mut t, _) = lower();
    let kinds = [
        Attributes::normal(Access::USER_READ),
        Attributes::normal(Access::USER_READ_WRITE),
        Attributes::normal(Access::USER_READ_EXECUTE),
        Attributes::normal(Access::KERNEL_READ),
        Attributes::normal(Access::KERNEL_READ_WRITE),
        Attributes::normal(Access::KERNEL_READ_EXECUTE),
        Attributes::non_cacheable(Access::KERNEL_READ_WRITE),
        Attributes::device(Access::KERNEL_READ_WRITE),
    ];
    for (i, a) in kinds.iter().enumerate() {
        let va = 0x1000_0000 + i * PAGE_SIZE;
        t.map(va, p(0x8000_0000 + i * PAGE_SIZE), PAGE_SIZE, *a, false).unwrap();
        assert_eq!(t.translate(va).unwrap().1, *a);
    }
}

#[test]
fn uses_blocks_when_aligned_and_allowed() {
    let (mut t, m) = lower();
    // 1 GiB + 2 MiB + 4 KiB, all suitably aligned.
    t.map(GIB, p(2 * GIB), GIB + MIB2 + PAGE_SIZE, RW, true).unwrap();
    // Root, then one level-2 table (for the 2 MiB block) and one level-3
    // table (for the page): the 1 GiB block needs no table.
    assert_eq!(m.live(), 3);
    assert_eq!(t.translate(GIB + 0x1234_5678).unwrap().0, p(2 * GIB + 0x1234_5678));
    assert_eq!(t.translate(2 * GIB + MIB2 + 5).unwrap().0, p(3 * GIB + MIB2 + 5));
    // The same range as pages would need many level-3 tables.
    let (mut pages, m2) = lower();
    pages.map(GIB, p(2 * GIB), 4 * MIB2, RW, false).unwrap();
    assert_eq!(m2.live(), 1 + 1 + 4);
}

#[test]
fn refuses_overlaps_and_leaves_nothing_behind() {
    let (mut t, _) = lower();
    t.map(0x10_0000, p(0x50_0000), 2 * PAGE_SIZE, RW, false).unwrap();
    // Overlaps the second page: fails, and its first page must not stay.
    let err = t.map(0xf_f000, p(0x90_0000), 3 * PAGE_SIZE, RW, false);
    assert_eq!(err, Err(MapError::AlreadyMapped));
    assert_eq!(t.translate(0xf_f000), None);
    assert_eq!(t.translate(0x10_0000).unwrap().0, p(0x50_0000), "original mapping untouched");
    // A block over existing pages is refused too.
    assert_eq!(t.map(0, p(0), MIB2, RW, true), Err(MapError::AlreadyMapped));
}

#[test]
fn running_out_of_table_memory_rolls_back() {
    let (mut t, m) = lower();
    m.limit(3); // root + one level-2 + one level-3
    let err = t.map(0, p(0), 2 * MIB2, RW, false); // needs a second level-3 table
    assert_eq!(err, Err(MapError::OutOfMemory));
    assert_eq!(t.translate(0), None);
    assert_eq!(t.translate(MIB2 - PAGE_SIZE), None);
}

#[test]
fn checks_alignment_and_address_range() {
    let (mut t, _) = lower();
    assert_eq!(t.map(0x1001, p(0), PAGE_SIZE, RW, false), Err(MapError::Misaligned));
    assert_eq!(t.map(0x1000, p(0x10), PAGE_SIZE, RW, false), Err(MapError::Misaligned));
    assert_eq!(t.map(0x1000, p(0), 100, RW, false), Err(MapError::Misaligned));
    assert_eq!(t.map(HALF_SIZE, p(0), PAGE_SIZE, RW, false), Err(MapError::OutOfRange));
    assert_eq!(t.map(HALF_SIZE - PAGE_SIZE, p(0), 2 * PAGE_SIZE, RW, false), Err(MapError::OutOfRange));
    assert_eq!(t.map(UPPER_HALF_START, p(0), PAGE_SIZE, RW, false), Err(MapError::OutOfRange));
}

#[test]
fn upper_half_tables_take_kernel_addresses() {
    let m = FakeMemory::default();
    let mut t = PageTable::new(Half::Upper, m).unwrap();
    let kernel = UPPER_HALF_START + 0x8_0000;
    let code = Attributes::normal(Access::KERNEL_READ_EXECUTE);
    t.map(kernel, p(0x8_0000), PAGE_SIZE, code, false).unwrap();
    assert_eq!(t.translate(kernel + 8), Some((p(0x8_0008), code)));
    assert_eq!(t.map(0x8_0000, p(0x8_0000), PAGE_SIZE, code, false), Err(MapError::OutOfRange));
    assert_eq!(t.translate(0x8_0000), None);
}

#[test]
fn unmaps_pages_and_whole_blocks_only() {
    let (mut t, _) = lower();
    t.map(0, p(0), 4 * PAGE_SIZE, RW, false).unwrap();
    t.unmap(PAGE_SIZE, 2 * PAGE_SIZE).unwrap();
    assert!(t.translate(0).is_some() && t.translate(3 * PAGE_SIZE).is_some());
    assert!(t.translate(PAGE_SIZE).is_none() && t.translate(2 * PAGE_SIZE).is_none());
    assert_eq!(t.unmap(PAGE_SIZE, PAGE_SIZE), Err(MapError::NotMapped));

    t.map(GIB, p(GIB), MIB2, RW, true).unwrap(); // one 2 MiB block
    assert_eq!(t.unmap(GIB, PAGE_SIZE), Err(MapError::SplitsBlock));
    t.unmap(GIB, MIB2).unwrap();
    assert!(t.translate(GIB).is_none());
}

#[test]
fn dropping_frees_every_table() {
    let m = FakeMemory::default();
    {
        let mut t = PageTable::new(Half::Lower, m.clone()).unwrap();
        t.map(0, p(0), 3 * MIB2, RW, false).unwrap();
        t.map(5 * GIB, p(0), PAGE_SIZE, RW, false).unwrap();
        assert!(m.live() > 4);
    }
    assert_eq!(m.live(), 0);
}

#[test]
fn randomized_against_a_model() {
    let (mut t, _) = lower();
    let mut model: HashMap<usize, usize> = HashMap::new(); // page -> frame
    let mut rng = 0x0123_4567_89ab_cdefu64;
    let mut next = |n: usize| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % n as u64) as usize
    };
    for _ in 0..20_000 {
        // A small window of pages that crosses a 2 MiB boundary.
        let page = 480 + next(64);
        let count = 1 + next(4);
        let va = page * PAGE_SIZE;
        let all_free = (page..page + count).all(|q| !model.contains_key(&q));
        let all_used = (page..page + count).all(|q| model.contains_key(&q));
        if next(2) == 0 {
            let frame = next(1 << 20) * PAGE_SIZE;
            let result = t.map(va, p(frame), count * PAGE_SIZE, RW, false);
            assert_eq!(result.is_ok(), all_free);
            if all_free {
                for i in 0..count {
                    model.insert(page + i, frame + i * PAGE_SIZE);
                }
            }
        } else {
            let result = t.unmap(va, count * PAGE_SIZE);
            assert_eq!(result.is_ok(), all_used || count == 0);
            if result.is_ok() {
                for i in 0..count {
                    model.remove(&(page + i));
                }
            }
        }
        for q in 470..560 {
            assert_eq!(t.translate(q * PAGE_SIZE).map(|(pa, _)| pa.as_usize()), model.get(&q).copied(), "page {q}");
        }
    }
}
