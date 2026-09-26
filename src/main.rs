//! pios: a small command-line OS for the Raspberry Pi 4 and 5.
//!
//! This is currently the Rust version of the OSDev "Raspberry Pi Bare Bones"
//! tutorial: bring up a serial console, say hello, and echo back whatever is
//! typed.

#![no_std]
#![no_main]
// Register maps read better with explicit `BASE + 0x00` offsets.
#![allow(clippy::identity_op)]

mod board;
mod console;
mod cpu;
mod exception;
mod gpio;
mod mailbox;
mod mmio;
mod timer;
mod uart;

use core::panic::PanicInfo;

use board::Model;

// The entry point, `_start`, and the exception vectors live in assembly.
// `_start` parks the secondary cores, sets up a stack, clears .bss and then
// calls `kernel_main`.
core::arch::global_asm!(include_str!("boot.s"));

unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

/// Called from `_start` on the primary core with the device tree address.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(dtb: usize) -> ! {
    // Without knowing the board we don't know where any UART is, so there is
    // nothing useful we can do.
    let Some(model) = Model::detect() else { halt() };

    let uarts = board::init_console(model);

    println!();
    println!("Hello, world!");
    println!();
    println!("pios {} on {}", env!("CARGO_PKG_VERSION"), model.name());
    let (_, _, variant, revision) = cpu::id();
    println!(
        "  CPU             : {} r{}p{}, running at EL{}",
        cpu::name(),
        variant,
        revision,
        cpu::current_el()
    );
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
    for uart in uarts.iter().flatten() {
        match uart.clock_hz {
            Some(hz) => println!(
                "  console         : {}, {} baud ({} Hz clock)",
                uart.name,
                uart::BAUD_RATE,
                hz
            ),
            None => println!(
                "  console         : {} (as set up by the firmware)",
                uart.name
            ),
        }
    }

    let mailbox = model.mailbox();
    let mut revision = [0u32; 1];
    match mailbox.property(mailbox::TAG_GET_BOARD_REVISION, &mut revision) {
        Some(()) => println!("  board revision  : {:#08x}", revision[0]),
        None => println!("  board revision  : (no answer from the firmware)"),
    }
    let mut memory = [0u32; 2];
    if mailbox
        .property(mailbox::TAG_GET_ARM_MEMORY, &mut memory)
        .is_some()
    {
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
        match console::getc() {
            b'\r' | b'\n' => console::puts("\n"),
            c => console::putc(c),
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

/// Stop this core for good.
pub fn halt() -> ! {
    loop {
        // SAFETY: `wfe` just idles the core until an event arrives.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("\n*** KERNEL PANIC: {}", info.message());
    if let Some(location) = info.location() {
        println!("    at {}", location);
    }
    halt()
}
