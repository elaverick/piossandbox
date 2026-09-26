//! Handles to hardware, so drivers can run in user space: memory (device
//! registers, a framebuffer, or read-only data), interrupts and timers.
//!
//! Interrupts and timers reach their owner as notifications on an endpoint
//! it chooses (see `ipc.rs`). An interrupt is masked each time it arrives,
//! until its owner acknowledges it, having dealt with the device: otherwise
//! a level-triggered interrupt would fire again at once, for ever.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use pios_abi::rights;

use crate::addr::{PAGE_SIZE, PhysAddr};
use crate::ipc::EndpointRef;
use crate::irq;
use crate::paging::{Access, Attributes};
use crate::sync::SpinLock;
use crate::timer;

/// Memory a handle lets its holder map.
#[derive(Clone, Copy)]
pub enum Memory {
    /// Physical memory the kernel doesn't use as RAM, mapped read-write
    /// with `attributes` (device or non-cacheable).
    Physical {
        base: PhysAddr,
        size: usize,
        attributes: Attributes,
    },
    /// Data the kernel keeps for good, mapped read-only.
    Static(&'static [u8]),
}

impl Memory {
    /// Device registers at `base`, `size` bytes (both page-aligned).
    ///
    /// # Safety
    ///
    /// The range must be device registers (or other memory that isn't RAM
    /// the kernel or anyone else uses) that are safe to give a process.
    pub unsafe fn device(base: PhysAddr, size: usize) -> Memory {
        Memory::physical(base, size, Attributes::device(Access::USER_READ_WRITE))
    }

    /// A framebuffer at `base`, `size` bytes, mapped non-cacheable so the
    /// GPU sees what is drawn.
    ///
    /// # Safety
    ///
    /// As for `device`: the range must be the GPU's framebuffer, which the
    /// frame allocator never hands out.
    pub unsafe fn framebuffer(base: PhysAddr, size: usize) -> Memory {
        Memory::physical(
            base,
            size,
            Attributes::non_cacheable(Access::USER_READ_WRITE),
        )
    }

    fn physical(base: PhysAddr, size: usize, attributes: Attributes) -> Memory {
        assert!(base.as_usize().is_multiple_of(PAGE_SIZE));
        Memory::Physical {
            base,
            size: size.next_multiple_of(PAGE_SIZE),
            attributes,
        }
    }

    /// Its size in bytes (a whole number of pages).
    pub fn size(&self) -> usize {
        match self {
            Memory::Physical { size, .. } => *size,
            Memory::Static(data) => data.len(),
        }
    }
}

/// A copy of `data` in whole pages of kernel memory that is never freed,
/// for sharing read-only with user space.
pub fn leak_pages(data: &[u8]) -> &'static [u8] {
    let size = data.len().max(1).next_multiple_of(PAGE_SIZE);
    let layout = core::alloc::Layout::from_size_align(size, PAGE_SIZE).expect("valid layout");
    // SAFETY: the layout has a nonzero size.
    let pages = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if pages.is_null() {
        alloc::alloc::handle_alloc_error(layout);
    }
    // SAFETY: `pages` is a fresh allocation of `size` >= `data.len()` bytes,
    // never freed, so it can be borrowed for ever once written.
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), pages, data.len());
        core::slice::from_raw_parts(pages, size)
    }
}

/// Where each owned interrupt's notifications go.
struct Binding {
    endpoint: EndpointRef,
    badge: u64,
}

/// Interrupts owned through handles, by ID, with where they are delivered
/// (`None` until bound).
static INTERRUPTS: SpinLock<BTreeMap<u32, Option<Binding>>> = SpinLock::new(BTreeMap::new());

/// Ownership of one interrupt. Dropping it gives the interrupt up (masked).
pub struct Interrupt {
    id: u32,
}

impl Interrupt {
    /// Take interrupt `id` away from the kernel, masked, for a handle.
    /// Returns `None` if it is already owned.
    pub fn claim(id: u32) -> Option<Interrupt> {
        assert!(irq::valid(id));
        let mut interrupts = INTERRUPTS.lock();
        if interrupts.contains_key(&id) {
            return None;
        }
        irq::set_enabled(id, false);
        irq::set_handler(id, user_interrupt);
        interrupts.insert(id, None);
        Some(Interrupt { id })
    }

    /// Deliver it to `endpoint` with `badge`, and unmask it.
    pub fn bind(&self, endpoint: EndpointRef, badge: u64) {
        let old = INTERRUPTS
            .lock()
            .insert(self.id, Some(Binding { endpoint, badge }));
        irq::set_enabled(self.id, true);
        // (Any old binding is dropped here, without the lock.)
        drop(old);
    }

    /// The owner has dealt with it: unmask it, if it is bound.
    pub fn acknowledge(&self) {
        if matches!(INTERRUPTS.lock().get(&self.id), Some(Some(_))) {
            irq::set_enabled(self.id, true);
        }
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        irq::set_enabled(self.id, false);
        let binding = INTERRUPTS.lock().remove(&self.id);
        drop(binding);
    }
}

/// The interrupt handler for every owned interrupt: mask it and notify its
/// owner.
fn user_interrupt(id: u32) {
    irq::set_enabled(id, false);
    if let Some(Some(binding)) = INTERRUPTS.lock().get(&id) {
        binding.endpoint.endpoint().notify(binding.badge);
    }
}

struct TimerEntry {
    id: u64,
    endpoint: EndpointRef,
    badge: u64,
    /// In timer ticks.
    period: u64,
    next: u64,
}

static TIMERS: SpinLock<Vec<TimerEntry>> = SpinLock::new(Vec::new());
static NEXT_TIMER: AtomicU64 = AtomicU64::new(1);

/// A periodic notification. Dropping it stops it.
pub struct Timer {
    id: u64,
}

impl Timer {
    /// Notify `endpoint` with `badge` every `period_ms` milliseconds
    /// (rounded up to whole ticks).
    pub fn start(endpoint: EndpointRef, badge: u64, period_ms: u64) -> Timer {
        let period = period_ms
            .saturating_mul(timer::TICK_HZ)
            .div_ceil(1000)
            .max(1);
        let id = NEXT_TIMER.fetch_add(1, Ordering::Relaxed);
        let entry = TimerEntry {
            id,
            endpoint,
            badge,
            period,
            next: timer::tick_count() + period,
        };
        TIMERS.lock().push(entry);
        Timer { id }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let removed = {
            let mut timers = TIMERS.lock();
            let index = timers.iter().position(|t| t.id == self.id);
            index.map(|i| timers.swap_remove(i))
        };
        drop(removed);
    }
}

/// Called on every timer tick (in interrupt context): send the timers'
/// notifications that are due.
pub fn tick() {
    let now = timer::tick_count();
    for entry in TIMERS.lock().iter_mut() {
        if now >= entry.next {
            entry.endpoint.endpoint().notify(entry.badge);
            entry.next = now + entry.period;
        }
    }
}

/// What handles to devices allow.
pub const MEMORY_RIGHTS: usize = rights::MAP | rights::TRANSFER;
