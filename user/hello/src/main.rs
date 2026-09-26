//! The first pios user program.

#![no_std]
#![no_main]

use libpios::println;

libpios::pios_main!(main);

/// Something to compute, to show code and data working in user space.
static PRIMES_BELOW: u32 = 100;

fn main() -> i32 {
    // init starts us with a handle to the console server.
    if let Some(console) = libpios::start_handle() {
        libpios::console::connect(console);
    }
    println!("Hello from user space!");
    let mut primes = [0u32; 32];
    let mut count = 0;
    for n in 2..PRIMES_BELOW {
        if primes[..count].iter().all(|&p| n % p != 0) {
            primes[count] = n;
            count += 1;
        }
    }
    println!("  {} primes below {}: {:?}", count, PRIMES_BELOW, &primes[..count]);
    println!("  (running at EL0, stack near {:#x})", &count as *const usize as usize);
    0
}
