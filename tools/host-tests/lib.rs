//! Host unit tests for src/allocator.rs, which is compiled in unchanged. Run
//! them with `make test-host`. (The main crate is built for the Pi, so these
//! live in their own crate, built for the host.)

#[path = "../../src/allocator.rs"]
pub mod allocator;

#[cfg(test)]
mod tests {
    use super::allocator::{Heap, MIN_BLOCK};
    use std::alloc::Layout;

    /// A heap over a buffer of `size` bytes from the host allocator.
    struct TestHeap {
        heap: Heap,
        memory: *mut u8,
        layout: Layout,
    }

    impl TestHeap {
        fn new(size: usize) -> Self {
            let layout = Layout::from_size_align(size, 4096).unwrap();
            // SAFETY: non-zero size.
            let memory = unsafe { std::alloc::alloc(layout) };
            assert!(!memory.is_null());
            let mut heap = Heap::empty();
            // SAFETY: we own `memory` for the life of the TestHeap.
            unsafe { heap.init(memory as usize, size) };
            TestHeap { heap, memory, layout }
        }

        fn base(&self) -> usize {
            self.memory as usize
        }
    }

    impl Drop for TestHeap {
        fn drop(&mut self) {
            // SAFETY: allocated in `new` with this layout.
            unsafe { std::alloc::dealloc(self.memory, self.layout) };
        }
    }

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn fresh_heap_is_one_free_block() {
        let t = TestHeap::new(64 * 1024);
        let s = t.heap.stats();
        assert_eq!(s.total, 64 * 1024);
        assert_eq!(s.used, 0);
        assert_eq!(s.free, 64 * 1024);
        assert_eq!(s.free_blocks, 1);
    }

    #[test]
    fn sizes_are_rounded_to_min_block() {
        let mut t = TestHeap::new(4096);
        let p = t.heap.allocate(layout(1, 1)).unwrap();
        assert_eq!(t.heap.stats().used, MIN_BLOCK);
        assert_eq!(p.as_ptr() as usize % MIN_BLOCK, 0);
    }

    #[test]
    fn first_fit_reuses_freed_memory() {
        let mut t = TestHeap::new(4096);
        let a = t.heap.allocate(layout(64, 8)).unwrap();
        let _b = t.heap.allocate(layout(64, 8)).unwrap();
        unsafe { t.heap.deallocate(a, layout(64, 8)) };
        let c = t.heap.allocate(layout(32, 8)).unwrap();
        assert_eq!(a, c, "the freed block at the front should be reused first");
    }

    #[test]
    fn honours_large_alignments() {
        let mut t = TestHeap::new(256 * 1024);
        // Knock the next free address off any large alignment first.
        let _pad = t.heap.allocate(layout(48, 16)).unwrap();
        for align in [32, 64, 256, 4096, 65536] {
            let p = t.heap.allocate(layout(100, align)).unwrap();
            assert_eq!(p.as_ptr() as usize % align, 0, "alignment {align}");
        }
    }

    #[test]
    fn out_of_memory_returns_none() {
        let mut t = TestHeap::new(4096);
        assert!(t.heap.allocate(layout(8192, 16)).is_none());
        assert!(t.heap.allocate(layout(isize::MAX as usize - 32, 16)).is_none());
        let p = t.heap.allocate(layout(4096, 16)).unwrap();
        assert!(t.heap.allocate(layout(1, 1)).is_none());
        unsafe { t.heap.deallocate(p, layout(4096, 16)) };
        assert_eq!(t.heap.stats().free, 4096);
    }

    #[test]
    fn freeing_in_any_order_coalesces_back_to_one_block() {
        for order in [[0, 1, 2, 3], [3, 2, 1, 0], [1, 3, 0, 2], [2, 0, 3, 1]] {
            let mut t = TestHeap::new(4096);
            let l = layout(100, 16);
            let ptrs: Vec<_> = (0..4).map(|_| t.heap.allocate(l).unwrap()).collect();
            for i in order {
                unsafe { t.heap.deallocate(ptrs[i], l) };
            }
            let s = t.heap.stats();
            assert_eq!((s.used, s.free_blocks, s.largest_free), (0, 1, 4096), "order {order:?}");
        }
    }

    #[test]
    fn unaligned_region_is_trimmed() {
        let t = TestHeap::new(4096 + 64);
        let mut heap = Heap::empty();
        unsafe { heap.init(t.base() + 3, 4096) };
        let (start, end) = heap.region();
        assert_eq!(start % MIN_BLOCK, 0);
        assert_eq!(end % MIN_BLOCK, 0);
        assert!(start >= t.base() + 3 && end <= t.base() + 3 + 4096);
    }

    /// xorshift64: deterministic pseudo-random numbers.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Random allocations and frees, checking every block against the
    /// others: within the heap, aligned, never overlapping, and contents
    /// untouched until freed. Then everything must coalesce back.
    #[test]
    fn randomized_stress() {
        const SIZE: usize = 1 << 20;
        let mut t = TestHeap::new(SIZE);
        let (start, end) = t.heap.region();
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut live: Vec<(usize, Layout, u8)> = Vec::new();

        for step in 0..200_000u32 {
            let allocate = live.is_empty() || (live.len() < 500 && rng.below(100) < 55);
            if allocate {
                let size = if rng.below(10) == 0 { 1 + rng.below(16384) } else { 1 + rng.below(256) } as usize;
                let align = 1usize << rng.below(9); // 1 ..= 256
                let l = layout(size, align);
                let Some(p) = t.heap.allocate(l) else { continue };
                let addr = p.as_ptr() as usize;
                assert_eq!(addr % align, 0, "step {step}");
                assert!(addr >= start && addr + size <= end, "step {step}: outside the heap");
                for &(other, ol, _) in &live {
                    assert!(addr + size <= other || other + ol.size() <= addr, "step {step}: overlap");
                }
                let fill = step as u8;
                unsafe { std::ptr::write_bytes(p.as_ptr(), fill, size) };
                live.push((addr, l, fill));
            } else {
                let (addr, l, fill) = live.swap_remove(rng.below(live.len() as u64) as usize);
                let bytes = unsafe { std::slice::from_raw_parts(addr as *const u8, l.size()) };
                assert!(bytes.iter().all(|&b| b == fill), "step {step}: contents changed");
                unsafe { t.heap.deallocate(std::ptr::NonNull::new(addr as *mut u8).unwrap(), l) };
            }
            let s = t.heap.stats();
            assert_eq!(s.used + s.free, s.total, "step {step}: bytes lost");
        }

        for (addr, l, _) in live.drain(..) {
            unsafe { t.heap.deallocate(std::ptr::NonNull::new(addr as *mut u8).unwrap(), l) };
        }
        let s = t.heap.stats();
        assert_eq!((s.used, s.free_blocks, s.free), (0, 1, s.total));
    }
}
