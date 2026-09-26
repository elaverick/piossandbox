//! Tries to read kernel memory. The kernel must stop it with a fault, and
//! carry on itself. Run by the kernel's boot self-test.

#![no_std]
#![no_main]

libpios::pios_main!(main);

fn main() -> i32 {
    let kernel = 0xFFFF_FF80_0008_0000 as *const u32;
    // SAFETY: none: this is meant to fault.
    #[allow(clippy::undocumented_unsafe_blocks)]
    let value = unsafe { kernel.read_volatile() };
    libpios::println!("crashtest: read kernel memory: {:#x}", value);
    0
}
