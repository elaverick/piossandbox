//! Exception handling. Nothing is handled yet: any exception is reported with
//! its syndrome and the kernel stops, which beats a silent hang when bringing
//! up real hardware.

use core::arch::asm;

use crate::cpu;

/// Called by the vector table in boot.s. `index` is the vector entry taken
/// (0-15): bits [3:2] say where the exception came from and bits [1:0] what
/// kind it was.
#[unsafe(no_mangle)]
extern "C" fn fatal_exception(index: u64) -> ! {
    const KIND: [&str; 4] = ["synchronous", "IRQ", "FIQ", "SError"];
    const SOURCE: [&str; 4] = [
        "EL0 stack",
        "current EL",
        "lower EL (AArch64)",
        "lower EL (AArch32)",
    ];

    let (esr, elr, far) = syndrome();
    crate::println!(
        "\n*** UNHANDLED EXCEPTION: {} exception from {}: {}\n    ESR {:#010x}  ELR {:#x}  FAR {:#x}",
        KIND[(index & 3) as usize],
        SOURCE[((index >> 2) & 3) as usize],
        exception_class(esr),
        esr,
        elr,
        far,
    );
    crate::halt()
}

/// Read ESR, ELR and FAR for the exception level we are running at.
fn syndrome() -> (u64, u64, u64) {
    let (esr, elr, far): (u64, u64, u64);
    // SAFETY: reading these system registers at our own EL has no side
    // effects.
    unsafe {
        match cpu::current_el() {
            3 => asm!("mrs {}, esr_el3", "mrs {}, elr_el3", "mrs {}, far_el3",
                      out(reg) esr, out(reg) elr, out(reg) far, options(nomem, nostack)),
            2 => asm!("mrs {}, esr_el2", "mrs {}, elr_el2", "mrs {}, far_el2",
                      out(reg) esr, out(reg) elr, out(reg) far, options(nomem, nostack)),
            _ => asm!("mrs {}, esr_el1", "mrs {}, elr_el1", "mrs {}, far_el1",
                      out(reg) esr, out(reg) elr, out(reg) far, options(nomem, nostack)),
        }
    }
    (esr, elr, far)
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
