//! Exception handling at EL1.
//!
//! The vector table in boot.s saves the interrupted state as a `TrapFrame`
//! and calls `exception_handler`. When the handler returns, the (possibly
//! modified) frame is restored and execution resumes at `frame.elr`.

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
    _reserved: u64,
}

const _: () = assert!(core::mem::size_of::<TrapFrame>() == 288);

// The kind of exception: bits [1:0] of the vector index.
const SYNCHRONOUS: u64 = 0;
const IRQ: u64 = 1;

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
    match index & 3 {
        IRQ => crate::irq::handle(),
        SYNCHRONOUS => match frame.esr >> 26 {
            EC_SVC64 => {
                // No system calls exist yet. As a placeholder, `svc #n`
                // returns x0 + n, which the boot self-test uses to check that
                // an exception can modify registers and return. ELR already
                // points after the `svc`.
                let imm = frame.esr & 0xFFFF;
                frame.x[0] = frame.x[0].wrapping_add(imm);
            }
            EC_BRK64 => {
                // Step over the breakpoint. (Only one core takes exceptions
                // for now, so a plain load and store is enough.)
                BREAKPOINTS.store(breakpoints() + 1, Ordering::Relaxed);
                frame.elr += 4;
            }
            _ => fatal(frame, index),
        },
        _ => fatal(frame, index),
    }
}

/// Report an exception we can't handle, with the full register state, and
/// stop.
fn fatal(frame: &TrapFrame, index: u64) -> ! {
    const KIND: [&str; 4] = ["synchronous", "IRQ", "FIQ", "SError"];
    const SOURCE: [&str; 4] = ["EL1 (SP_EL0)", "EL1", "EL0 (AArch64)", "EL0 (AArch32)"];

    crate::println!(
        "\n*** UNHANDLED EXCEPTION: {} exception from {}: {}",
        KIND[(index & 3) as usize],
        SOURCE[((index >> 2) & 3) as usize],
        exception_class(frame.esr),
    );
    crate::println!(
        "    ESR  {:#010x}   ELR {:#018x}   FAR {:#018x}   SPSR {:#010x}",
        frame.esr,
        frame.elr,
        frame.far,
        frame.spsr
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

/// Describe the exception class field (ESR bits [31:26]).
fn exception_class(esr: u64) -> &'static str {
    match esr >> 26 {
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
    }
}
