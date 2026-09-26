//! Kernel stacks for threads.
//!
//! Each thread gets its own kernel stack, mapped above the linear map in a
//! slot of its own with an unmapped guard below it. Running off the bottom
//! of the stack faults on the guard instead of silently overwriting whatever
//! lies below, and the exception entry code (boot.s) reports it.

use alloc::vec::Vec;

use crate::addr::{KERNEL_BASE, LINEAR_MAP_SIZE, PAGE_SIZE, PhysAddr};
use crate::memory::OwnedFrame;
use crate::mmu;
use crate::sync::SpinLock;

/// The usable size of a kernel stack.
pub const STACK_SIZE: usize = 16 * 1024;
const STACK_PAGES: usize = STACK_SIZE / PAGE_SIZE;

/// Each stack's slot: the stack at the top, an equal-sized guard below.
const SLOT_SIZE: usize = 2 * STACK_SIZE;
const STACKS_START: usize = KERNEL_BASE + LINEAR_MAP_SIZE;
const MAX_SLOTS: usize = LINEAR_MAP_SIZE / SLOT_SIZE;

/// Slots handed out: `next` has never been used; `free` were given back
/// (reused first, so the page tables covering them get reused too).
struct Slots {
    next: usize,
    free: Vec<usize>,
}

static SLOTS: SpinLock<Slots> = SpinLock::new(Slots {
    next: 0,
    free: Vec::new(),
});

/// The lowest address of the stack in `slot`.
fn bottom(slot: usize) -> usize {
    STACKS_START + slot * SLOT_SIZE + (SLOT_SIZE - STACK_SIZE)
}

/// A kernel stack. Dropping it unmaps and frees it, so it must not be in use
/// by then.
pub struct KernelStack {
    slot: usize,
    /// The memory behind the stack, freed after the stack is unmapped.
    _frames: Vec<OwnedFrame>,
}

impl KernelStack {
    /// A new, zeroed stack, or `None` if there isn't the memory for one.
    pub fn new() -> Option<KernelStack> {
        let frames: Vec<OwnedFrame> = (0..STACK_PAGES)
            .map(|_| OwnedFrame::allocate())
            .collect::<Option<_>>()?;
        let slot = {
            let mut slots = SLOTS.lock();
            match slots.free.pop() {
                Some(slot) => slot,
                None if slots.next < MAX_SLOTS => {
                    slots.next += 1;
                    slots.next - 1
                }
                None => return None,
            }
        };
        let addrs: Vec<PhysAddr> = frames.iter().map(OwnedFrame::addr).collect();
        if mmu::map_kernel_pages(bottom(slot), &addrs).is_err() {
            SLOTS.lock().free.push(slot);
            return None;
        }
        Some(KernelStack {
            slot,
            _frames: frames,
        })
    }

    /// The lowest address of the stack.
    pub fn bottom(&self) -> usize {
        bottom(self.slot)
    }

    /// One past the highest address: the initial stack pointer.
    pub fn top(&self) -> usize {
        self.bottom() + STACK_SIZE
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        mmu::unmap_kernel_pages(self.bottom(), STACK_PAGES).expect("a kernel stack is mapped");
        SLOTS.lock().free.push(self.slot);
        // The frames are freed after this, now nothing maps them.
    }
}
