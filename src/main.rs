//! pios: a small command-line OS for the Raspberry Pi 4.
//!
//! This is currently the Rust version of the OSDev "Raspberry Pi Bare Bones"
//! tutorial: bring up UART0, say hello, and echo back whatever is typed.

#![no_std]
#![no_main]
// Register maps read better with explicit `BASE + 0x00` offsets.
#![allow(clippy::identity_op)]

mod gpio;
mod mailbox;
mod mmio;
mod uart;

use core::panic::PanicInfo;

// The entry point, `_start`, lives in assembly. It parks the secondary
// cores, sets up a stack, clears .bss and then calls `kernel_main`.
core::arch::global_asm!(include_str!("boot.s"));

unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

/// Called from `_start` on core 0 with the device tree address in `x0`.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(dtb: usize) -> ! {
    let uart_clock = uart::init();

    println!();
    println!("Hello, world!");
    println!();
    println!("pios {} for Raspberry Pi 4", env!("CARGO_PKG_VERSION"));
    println!("  exception level : EL{}", current_el());
    println!(
        "  device tree     : {:#010x} ({})",
        dtb,
        if is_device_tree(dtb) {
            "valid"
        } else {
            "not found"
        }
    );
    println!(
        "  kernel image    : {:#010x} - {:#010x}",
        &raw const __kernel_start as usize, &raw const __kernel_end as usize
    );
    println!(
        "  UART0 clock     : {} Hz, {} baud",
        uart_clock,
        uart::BAUD_RATE
    );

    let mut revision = [0u32; 1];
    if mailbox::property(mailbox::TAG_GET_BOARD_REVISION, &mut revision).is_some() {
        println!("  board revision  : {:#08x}", revision[0]);
    }
    let mut memory = [0u32; 2];
    if mailbox::property(mailbox::TAG_GET_ARM_MEMORY, &mut memory).is_some() {
        println!(
            "  ARM memory      : {:#010x} - {:#010x} ({} MiB)",
            memory[0],
            memory[0] as u64 + memory[1] as u64,
            memory[1] >> 20
        );
    }

    println!();
    println!("Type something and it will be echoed back.");

    loop {
        match uart::getc() {
            b'\r' | b'\n' => uart::puts("\n"),
            c => uart::putc(c),
        }
    }
}

/// Does `addr` point at a flattened device tree (big-endian magic 0xd00dfeed)?
fn is_device_tree(addr: usize) -> bool {
    const FDT_MAGIC: u32 = 0xD00D_FEED;
    // SAFETY: the firmware passes either 0 or the address of the DTB it
    // loaded into RAM; we only read one aligned word from it.
    addr != 0
        && addr.is_multiple_of(4)
        && u32::from_be(unsafe { (addr as *const u32).read_volatile() }) == FDT_MAGIC
}

/// The exception level we are running at (the Pi 4 firmware enters at EL2).
fn current_el() -> u64 {
    let el: u64;
    // SAFETY: reading CurrentEL has no side effects and is allowed at EL1+.
    unsafe { core::arch::asm!("mrs {}, CurrentEL", out(reg) el, options(nomem, nostack)) };
    (el >> 2) & 0b11
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("\n*** KERNEL PANIC: {}", info);
    loop {
        // SAFETY: `wfe` just idles the core until an event arrives.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}
