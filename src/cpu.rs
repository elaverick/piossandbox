//! Information about the CPU we are running on.

use core::arch::asm;

/// The exception level we are running at (the Pi firmware enters at EL2).
pub fn current_el() -> u64 {
    let el: u64;
    // SAFETY: reading CurrentEL has no side effects and is allowed at EL1+.
    unsafe { asm!("mrs {}, CurrentEL", out(reg) el, options(nomem, nostack)) };
    (el >> 2) & 0b11
}

/// The Main ID Register, identifying the CPU design and revision.
pub fn midr() -> u64 {
    let midr: u64;
    // SAFETY: reading MIDR_EL1 has no side effects.
    unsafe { asm!("mrs {}, midr_el1", out(reg) midr, options(nomem, nostack)) };
    midr
}

pub const IMPLEMENTER_ARM: u64 = 0x41;
pub const PART_CORTEX_A72: u64 = 0xD08;
pub const PART_CORTEX_A76: u64 = 0xD0B;

/// (implementer, part number, variant, revision) from MIDR_EL1.
pub fn id() -> (u64, u64, u64, u64) {
    let midr = midr();
    (
        (midr >> 24) & 0xFF,
        (midr >> 4) & 0xFFF,
        (midr >> 20) & 0xF,
        midr & 0xF,
    )
}

/// A human-readable name for the CPU design.
pub fn name() -> &'static str {
    match id() {
        (IMPLEMENTER_ARM, PART_CORTEX_A72, ..) => "Cortex-A72",
        (IMPLEMENTER_ARM, PART_CORTEX_A76, ..) => "Cortex-A76",
        _ => "unknown CPU",
    }
}
