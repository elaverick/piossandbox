//! Interrupt dispatch: a table of handlers indexed by GIC interrupt ID.

use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};

use crate::addr::PhysAddr;
use crate::gic::{self, Gic};

const MAX_IDS: usize = 1020;

/// Handler for each interrupt ID (null if none). Plain loads and stores
/// only; handlers are registered before interrupts are enabled.
static HANDLERS: [AtomicPtr<()>; MAX_IDS] =
    [const { AtomicPtr::new(core::ptr::null_mut()) }; MAX_IDS];

/// Physical addresses of the GIC's two register blocks.
static DISTRIBUTOR: AtomicUsize = AtomicUsize::new(0);
static CPU_INTERFACE: AtomicUsize = AtomicUsize::new(0);
static UNEXPECTED: AtomicU32 = AtomicU32::new(0);

fn gic() -> Gic {
    Gic::new(
        PhysAddr::new(DISTRIBUTOR.load(Ordering::Relaxed)),
        PhysAddr::new(CPU_INTERFACE.load(Ordering::Relaxed)),
    )
}

/// Set up the interrupt controller, with every interrupt disabled. Returns
/// the number of interrupt IDs it supports.
pub fn init(distributor: PhysAddr, cpu_interface: PhysAddr) -> u32 {
    DISTRIBUTOR.store(distributor.as_usize(), Ordering::Relaxed);
    CPU_INTERFACE.store(cpu_interface.as_usize(), Ordering::Relaxed);
    gic().init()
}

/// Call `handler` (in interrupt context, with the interrupt's ID) whenever
/// interrupt `id` fires, and enable it.
pub fn register(id: u32, handler: fn(u32)) {
    set_handler(id, handler);
    gic().enable(id);
}

/// Make `handler` the one for interrupt `id`, leaving it enabled or not.
pub fn set_handler(id: u32, handler: fn(u32)) {
    HANDLERS[id as usize].store(handler as *mut (), Ordering::Release);
}

/// Let interrupt `id` through, or not, at the interrupt controller.
pub fn set_enabled(id: u32, enabled: bool) {
    if enabled {
        gic().enable(id);
    } else {
        gic().disable(id);
    }
}

/// Whether `id` is a valid interrupt ID.
pub fn valid(id: u32) -> bool {
    (id as usize) < MAX_IDS
}

/// Unmask IRQs on this core.
pub fn enable() {
    // SAFETY: handlers are registered and the vector table is installed.
    unsafe { core::arch::asm!("msr daifclr, #2", options(nomem, nostack)) };
}

/// Mask IRQs on this core.
pub fn disable() {
    // SAFETY: masking interrupts is always safe.
    unsafe { core::arch::asm!("msr daifset, #2", options(nomem, nostack)) };
}

/// Mask IRQs on this core, returning the previous state for `restore`.
pub fn save_and_disable() -> u64 {
    let daif: u64;
    // SAFETY: reading DAIF and masking IRQs are always safe.
    unsafe {
        core::arch::asm!("mrs {}, daif", "msr daifset, #2", out(reg) daif, options(nomem, nostack))
    };
    daif
}

/// Put back the interrupt mask saved by `save_and_disable`.
pub fn restore(daif: u64) {
    // SAFETY: restores mask bits previously read from DAIF.
    unsafe { core::arch::asm!("msr daif, {}", in(reg) daif, options(nomem, nostack)) };
}

/// Run `f` with IRQs masked on this core, restoring the previous state.
pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    let daif = save_and_disable();
    let result = f();
    restore(daif);
    result
}

/// How many interrupts arrived that nobody had registered for.
pub fn unexpected() -> u32 {
    UNEXPECTED.load(Ordering::Relaxed)
}

/// Called from the exception handler for an IRQ: handle every pending
/// interrupt.
pub fn handle() {
    let gic = gic();
    loop {
        let iar = gic.acknowledge();
        let id = iar & 0x3FF;
        if id >= gic::SPURIOUS {
            break;
        }
        let handler = HANDLERS[id as usize].load(Ordering::Acquire);
        if handler.is_null() {
            // Nobody wants it: switch it off rather than take it forever.
            gic.disable(id);
            UNEXPECTED.store(unexpected() + 1, Ordering::Relaxed);
        } else {
            // SAFETY: only `set_handler` stores into HANDLERS, always a
            // `fn(u32)`.
            let handler: fn(u32) = unsafe { core::mem::transmute(handler) };
            handler(id);
        }
        gic.end(iar);
    }
}
