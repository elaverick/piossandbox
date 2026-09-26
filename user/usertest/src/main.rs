//! Checks the kernel's system call interface from user space: calls work,
//! and bad arguments are refused rather than trusted. Run by the kernel's
//! boot self-test; exits 0 if everything behaves, else prints what didn't
//! and exits 1.

#![no_std]
#![no_main]

use libpios::{Error, println, raw_syscall};
use pios_abi::call;

libpios::pios_main!(main);

/// Read-only data in its own segment, which must stay read-only.
static CONSTANT: u32 = 0x1234_5678;

/// Busy work long enough for timer interrupts to arrive while it runs,
/// with an answer known in advance: if an interrupt corrupted a register,
/// the sum would come out wrong.
fn sum_of_squares(n: u64) -> u64 {
    let mut sum = 0u64;
    let mut i = 0u64;
    while i < core::hint::black_box(n) {
        sum = sum.wrapping_add(i.wrapping_mul(i));
        i += 1;
    }
    sum
}

/// How many iterations of `sum_of_squares` to run.
const SPIN: u64 = 30_000_000;

fn main() -> i32 {
    let text = b"";
    let n = SPIN as u128;
    let squares = ((n - 1) * n * (2 * n - 1) / 6) as u64; // sum of i^2 for i < n
    let checks: [(&str, bool); 9] = [
        ("registers survive interrupts during a long computation", sum_of_squares(SPIN) == squares),
        ("an empty write succeeds", raw_syscall(call::DEBUG_WRITE, text.as_ptr() as usize, 0, 0) == Ok(0)),
        ("an unknown call is refused", raw_syscall(999, 0, 0, 0) == Err(Error::NoSuchCall)),
        ("a null pointer is refused", raw_syscall(call::DEBUG_WRITE, 0, 1, 0) == Err(Error::BadAddress)),
        (
            "a kernel pointer is refused",
            raw_syscall(call::DEBUG_WRITE, 0xFFFF_FF80_0008_0000, 16, 0) == Err(Error::BadAddress),
        ),
        (
            "an unmapped pointer is refused",
            raw_syscall(call::DEBUG_WRITE, 0x7000_0000, 16, 0) == Err(Error::BadAddress),
        ),
        (
            "a range running off the end of memory is refused",
            raw_syscall(call::DEBUG_WRITE, usize::MAX - 4, 16, 0) == Err(Error::BadAddress),
        ),
        (
            "an oversized write is refused",
            raw_syscall(call::DEBUG_WRITE, text.as_ptr() as usize, pios_abi::DEBUG_WRITE_MAX + 1, 0)
                == Err(Error::InvalidArgument),
        ),
        ("read-only data is intact", CONSTANT == 0x1234_5678),
    ];
    let mut ok = true;
    for (what, passed) in checks {
        if !passed {
            println!("usertest: FAILED: {}", what);
            ok = false;
        }
    }
    if ok { 0 } else { 1 }
}
