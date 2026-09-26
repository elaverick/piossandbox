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
mod font;
mod framebuffer;
mod gic;
mod gpio;
mod irq;
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
    let mailbox = model.mailbox();
    let display = framebuffer::init(mailbox);

    // Interrupts: the timer tick and interrupt-driven serial input.
    let (gicd, gicc) = model.gic();
    let irq_lines = irq::init(gicd, gicc);
    irq::register(timer::TICK_IRQ, timer::handle_tick);
    timer::start_tick();
    console::enable_interrupts();
    irq::enable();

    let self_test = self_test();

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
            Some(hz) => print!(
                "  console         : {}, {} baud ({} Hz clock)",
                uart.name,
                uart::BAUD_RATE,
                hz
            ),
            None => print!(
                "  console         : {} (as set up by the firmware)",
                uart.name
            ),
        }
        match uart.irq {
            Some(id) => println!(", IRQ {}", id),
            None => println!(", polled"),
        }
    }
    println!(
        "  interrupts      : GIC-400 at {:#x} ({} IDs), {} Hz timer tick",
        gicd,
        irq_lines,
        timer::TICK_HZ
    );
    match self_test {
        Ok(()) => println!("  self-test       : svc, brk and timer interrupts OK"),
        Err(e) => println!("  self-test       : FAILED: {}", e),
    }

    match &display {
        Some(d) => {
            println!(
                "  display         : {}x{} framebuffer at {:#010x}, {}x{} characters",
                d.fb.width(),
                d.fb.height(),
                d.fb.base(),
                d.columns,
                d.rows
            );
            if let Some(n) = d.displays {
                println!("  displays found  : {}", n);
            }
        }
        None => println!("  display         : none (the firmware did not provide a framebuffer)"),
    }
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
            // Backspace or Delete: step back, blank the character, step back.
            0x08 | 0x7F => console::puts("\x08 \x08"),
            c => console::putc(c),
        }
    }
}

/// Check that exceptions return correctly and the timer interrupt is
/// running.
fn self_test() -> Result<(), &'static str> {
    // `svc #n` returns x0 + n (see exception.rs).
    let result: u64;
    // SAFETY: the exception handler returns from `svc` with only x0 changed.
    unsafe { core::arch::asm!("svc #0x42", inout("x0") 5u64 => result) };
    if result != 5 + 0x42 {
        return Err("svc did not return the expected value");
    }

    let before = exception::breakpoints();
    // SAFETY: the exception handler steps over `brk`.
    unsafe { core::arch::asm!("brk #1") };
    if exception::breakpoints() != before + 1 {
        return Err("brk was not handled");
    }

    // Over 50 ms, a 100 Hz tick should advance by about 5.
    let start = timer::tick_count();
    let deadline = timer::Deadline::after_us(50_000);
    while !deadline.expired() {
        core::hint::spin_loop();
    }
    let ticks = timer::tick_count() - start;
    if !(3..=7).contains(&ticks) {
        return Err("the timer tick is not running at the expected rate");
    }
    if irq::unexpected() != 0 {
        return Err("unexpected interrupts arrived");
    }
    Ok(())
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
    irq::disable();
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
