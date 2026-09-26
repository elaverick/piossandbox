//! Checks that the kernel keeps each thread's FP/SIMD registers to itself.
//!
//! Fills all 32 vector registers, FPCR and FPSR (and the thread pointer
//! register, TPIDR_EL0, also switched per thread) with values made from its
//! argument (the kernel's self-test starts two of these with different
//! arguments), spins long enough to be preempted, then checks nothing
//! changed. Repeats that a number of times, yielding in between. Exits 0 if
//! the registers always came back intact, else 1.

#![no_std]
#![no_main]

use libpios::println;

libpios::pios_main!(main);

/// How many times to fill, spin and check.
const ROUNDS: u64 = 6;
/// How long to spin in each round: a couple of time slices, so the timer
/// tick preempts us whatever the processor's speed.
const SPIN_MS: u64 = 25;

/// FPCR bits we set: the rounding mode.
const FPCR_RMODE: u64 = 3 << 22;
/// FPSR bits we set: the cumulative exception flags.
const FPSR_FLAGS: u64 = 0x1F;

/// Fill the registers from `seed`, spin for `spin` counter ticks, and
/// return the bits that differ from what was put in (0 if nothing changed).
fn fill_spin_check(seed: u64, spin: u64) -> u64 {
    let diff: u64;
    // SAFETY: the block only uses registers it declares, and leaves FPCR
    // with a valid (if unusual) rounding mode, which it restores to
    // round-to-nearest before returning.
    unsafe {
        core::arch::asm!(
            // v<n> = (seed << 8 | 2n, seed << 8 | 2n + 1)
            "lsl x9, {seed}, #8",
            ".irp n, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31",
            "add x10, x9, #(2 * \\n)",
            "add x11, x9, #(2 * \\n + 1)",
            "fmov d\\n, x10",
            "mov v\\n\\().d[1], x11",
            ".endr",
            "lsl x10, {seed}, #22",
            "and x10, x10, #{rmode}",
            "msr fpcr, x10",
            "and x11, {seed}, #{flags}",
            "msr fpsr, x11",
            "msr tpidr_el0, {seed}",
            // Spin, so the timer tick preempts us while the registers are
            // live.
            "isb",
            "mrs x12, cntvct_el0",
            "add x12, x12, {spin}",
            "2:",
            "isb",
            "mrs x14, cntvct_el0",
            "cmp x14, x12",
            "b.lo 2b",
            // Check: accumulate every difference in x13.
            "mrs x12, fpcr",
            "eor x13, x12, x10",
            "mrs x12, fpsr",
            "eor x12, x12, x11",
            "orr x13, x13, x12",
            "mrs x12, tpidr_el0",
            "eor x12, x12, {seed}",
            "orr x13, x13, x12",
            ".irp n, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31",
            "add x10, x9, #(2 * \\n)",
            "fmov x12, d\\n",
            "eor x12, x12, x10",
            "orr x13, x13, x12",
            "add x10, x9, #(2 * \\n + 1)",
            "mov x12, v\\n\\().d[1]",
            "eor x12, x12, x10",
            "orr x13, x13, x12",
            ".endr",
            "msr fpcr, xzr",
            "mov {diff}, x13",
            seed = in(reg) seed,
            spin = in(reg) spin,
            rmode = const FPCR_RMODE,
            flags = const FPSR_FLAGS,
            diff = lateout(reg) diff,
            out("x9") _, out("x10") _, out("x11") _, out("x12") _, out("x13") _, out("x14") _,
            out("v0") _, out("v1") _, out("v2") _, out("v3") _,
            out("v4") _, out("v5") _, out("v6") _, out("v7") _,
            out("v8") _, out("v9") _, out("v10") _, out("v11") _,
            out("v12") _, out("v13") _, out("v14") _, out("v15") _,
            out("v16") _, out("v17") _, out("v18") _, out("v19") _,
            out("v20") _, out("v21") _, out("v22") _, out("v23") _,
            out("v24") _, out("v25") _, out("v26") _, out("v27") _,
            out("v28") _, out("v29") _, out("v30") _, out("v31") _,
            options(nomem, nostack),
        );
    }
    diff
}

fn main() -> i32 {
    let seed = libpios::argument() as u64;
    let spin = libpios::counter_frequency() * SPIN_MS / 1000;
    for round in 0..ROUNDS {
        let diff = fill_spin_check(seed, spin);
        if diff != 0 {
            println!(
                "fptest {}: FAILED: registers changed in round {} (bits {:#x})",
                seed, round, diff
            );
            return 1;
        }
        if round % 2 == 1 {
            libpios::yield_now();
        }
    }
    0
}
