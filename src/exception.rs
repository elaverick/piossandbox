//! Exception handling at EL1.
//!
//! The vector table in boot.s saves the interrupted state as a `TrapFrame`
//! on the current thread's kernel stack and calls `exception_handler`. When
//! the handler returns, the (possibly modified) frame is restored and
//! execution resumes at `frame.elr`. The handler may switch threads first
//! (on a timer tick, or when a program exits), in which case this thread's
//! frame waits on its stack until the thread runs again.

use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};

/// The state saved on exception entry. Must match the layout in boot.s.
#[repr(C)]
pub struct TrapFrame {
    pub x: [u64; 31],
    /// Where execution resumes.
    pub elr: u64,
    /// The interrupted PSTATE, restored on return.
    pub spsr: u64,
    pub esr: u64,
    pub far: u64,
    /// The user stack pointer (SP_EL0).
    pub sp_el0: u64,
}

const _: () = assert!(core::mem::size_of::<TrapFrame>() == 288);

impl TrapFrame {
    /// The frame that starts a user thread: returning from it enters EL0 at
    /// `entry`, with interrupts enabled, the stack pointer at `stack`,
    /// `args` in x0-x3 and every other register zero.
    pub fn new_user(entry: u64, stack: u64, args: [u64; 4]) -> TrapFrame {
        let mut x = [0; 31];
        x[..4].copy_from_slice(&args);
        TrapFrame {
            x,
            elr: entry,
            spsr: 0, // EL0t, nothing masked
            esr: 0,
            far: 0,
            sp_el0: stack,
        }
    }
}

// The kind of exception: bits [1:0] of the vector index.
const SYNCHRONOUS: u64 = 0;
const IRQ: u64 = 1;

// Where it came from: bits [3:2] of the vector index. 3 would be a 32-bit
// user program, which we never run.
const FROM_USER: u64 = 2;

// Exception classes (ESR bits [31:26]) we handle.
const EC_SVC64: u64 = 0x15;
const EC_BRK64: u64 = 0x3C;

/// How many `brk` instructions have been stepped over.
static BREAKPOINTS: AtomicUsize = AtomicUsize::new(0);

pub fn breakpoints() -> usize {
    BREAKPOINTS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
extern "C" fn exception_handler(frame: &mut TrapFrame, index: u64) {
    let kind = index & 3;
    if kind == IRQ {
        crate::irq::handle();
        // If this was the end of a time slice, switch threads now the
        // interrupt is dealt with.
        crate::thread::preempt_if_needed();
        return;
    }
    if index >> 2 >= FROM_USER {
        // From a user program: a system call, or a fault that ends it.
        if index >> 2 == FROM_USER && kind == SYNCHRONOUS && frame.esr >> 26 == EC_SVC64 {
            // System calls can take a while (writing to a slow serial
            // port), so let interrupts, and other threads, in meanwhile.
            // The frame is safe on this thread's stack; returning from it
            // needs interrupts masked again.
            crate::irq::enable();
            crate::syscall::handle(frame);
            crate::irq::disable();
        } else {
            crate::process::user_fault(frame, index);
        }
        return;
    }
    match (kind, frame.esr >> 26) {
        (SYNCHRONOUS, EC_SVC64) => {
            // `svc` from the kernel itself: only the boot self-test does
            // this, to check an exception can change registers and return:
            // `svc #n` returns x0 + n. ELR already points after the `svc`.
            let imm = frame.esr & 0xFFFF;
            frame.x[0] = frame.x[0].wrapping_add(imm);
        }
        (SYNCHRONOUS, EC_BRK64) => {
            // Step over the breakpoint. (Only one core takes exceptions for
            // now, so a plain load and store is enough.)
            BREAKPOINTS.store(breakpoints() + 1, Ordering::Relaxed);
            frame.elr += 4;
        }
        _ => fatal(frame, index),
    }
}

/// Called by boot.s, on a spare stack, when an exception from the kernel
/// found the kernel stack full.
#[unsafe(no_mangle)]
extern "C" fn report_stack_overflow(elr: u64, far: u64, esr: u64) -> ! {
    crate::println!(
        "\n*** KERNEL STACK OVERFLOW in thread {}: {} at pc {:#x}, address {:#x}",
        crate::thread::Running,
        Syndrome(esr),
        elr,
        far
    );
    crate::halt()
}

pub const KIND: [&str; 4] = ["synchronous", "IRQ", "FIQ", "SError"];

/// Report an exception we can't handle, with the full register state, and
/// stop.
fn fatal(frame: &TrapFrame, index: u64) -> ! {
    const SOURCE: [&str; 4] = ["EL1 (SP_EL0)", "EL1", "EL0 (AArch64)", "EL0 (AArch32)"];

    crate::println!(
        "\n*** UNHANDLED EXCEPTION: {} exception from {} in thread {}: {}",
        KIND[(index & 3) as usize],
        SOURCE[((index >> 2) & 3) as usize],
        crate::thread::Running,
        Syndrome(frame.esr),
    );
    crate::println!(
        "    ESR  {:#010x}   ELR {:#018x}   FAR {:#018x}   SPSR {:#010x}   SP_EL0 {:#x}",
        frame.esr,
        frame.elr,
        frame.far,
        frame.spsr,
        frame.sp_el0
    );
    for row in 0..8 {
        crate::print!("   ");
        for reg in row * 4..(row * 4 + 4).min(31) {
            crate::print!(" x{:<2} {:016x}", reg, frame.x[reg]);
        }
        crate::println!();
    }
    crate::halt()
}

/// An exception syndrome (ESR) described in words: the exception class and,
/// for aborts, the kind of fault.
#[derive(Clone, Copy)]
pub struct Syndrome(pub u64);

impl Syndrome {
    pub fn is_data_abort(self) -> bool {
        matches!(self.0 >> 26, 0x24 | 0x25)
    }
}

impl fmt::Display for Syndrome {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let esr = self.0;
        let class = match esr >> 26 {
            0x00 => "unknown reason (e.g. undefined instruction)",
            0x01 => "trapped WFI/WFE",
            0x07 => "trapped FP/SIMD access",
            0x0E => "illegal execution state",
            0x15..=0x17 => "system call",
            0x18 => "trapped system register access",
            0x20 | 0x21 => "instruction abort",
            0x22 => "misaligned PC",
            0x24 | 0x25 => "data abort",
            0x26 => "misaligned stack pointer",
            0x2F => "SError",
            0x3C => "breakpoint (BRK)",
            _ => "other",
        };
        f.write_str(class)?;
        // For instruction and data aborts, say what kind of fault it was.
        if matches!(esr >> 26, 0x20 | 0x21 | 0x24 | 0x25) {
            const FAULTS: [&str; 4] = [
                "address size fault",
                "translation fault",
                "access flag fault",
                "permission fault",
            ];
            let status = esr & 0x3F;
            match status {
                0b000000..=0b001111 => write!(
                    f,
                    " ({}, level {})",
                    FAULTS[(status >> 2) as usize],
                    status & 3
                )?,
                0b010000 => f.write_str(" (external abort)")?,
                0b100001 => f.write_str(" (alignment fault)")?,
                _ => write!(f, " (fault status {:#x})", status)?,
            }
        }
        Ok(())
    }
}
