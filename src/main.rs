//! pios: a small command-line OS for the Raspberry Pi 4 and 5.
//!
//! This is currently the Rust version of the OSDev "Raspberry Pi Bare Bones"
//! tutorial: bring up a serial console, say hello, and echo back whatever is
//! typed.

#![no_std]
#![no_main]
// Every unsafe operation needs its own `unsafe` block, even inside an
// `unsafe fn`, and every `unsafe` block a `// SAFETY:` comment saying why it
// is sound.
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]
// Register maps read better with explicit `BASE + 0x00` offsets.
#![allow(clippy::identity_op)]

extern crate alloc;

#[allow(dead_code)] // shared with the host tests, which use more of it
mod addr;
mod addrspace;
#[allow(dead_code)] // shared with the host tests, which use more of it
mod allocator;
mod board;
mod bootimage;
mod cache;
mod console;
mod cpu;
#[allow(dead_code)] // shared with the host tests, which use more of it
mod elf;
mod exception;
#[allow(dead_code)] // shared with the host tests, which use more of it
mod fdt;
mod font;
mod framebuffer;
#[allow(dead_code)] // shared with the host tests, which use more of it
mod frames;
mod gic;
mod gpio;
mod handle;
mod heap;
mod ipc;
mod irq;
mod mailbox;
mod memory;
mod mmio;
mod mmu;
#[allow(dead_code)] // shared with the host tests, which use more of it
mod paging;
mod process;
#[allow(dead_code)] // shared with the host tests, which use more of it
mod ranges;
mod stack;
mod sync;
mod syscall;
mod thread;
mod timer;
mod uart;
mod user;

use core::panic::PanicInfo;

use addr::PhysAddr;
use board::Model;

// The entry point, `_start`, and the exception vectors live in assembly.
// `_start` parks the secondary cores, sets up a stack, clears .bss and then
// calls `kernel_main`.
core::arch::global_asm!(include_str!("boot.s"));

unsafe extern "C" {
    static __kernel_start: u8;
}

/// Called from `_start` on the primary core with the device tree address.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(dtb: usize) -> ! {
    // Without knowing the board we don't know where any UART is, so there is
    // nothing useful we can do.
    let Some(model) = Model::detect() else { halt() };

    let uarts = board::init_console(model);
    let mailbox = model.mailbox();

    // What the firmware can tell us about memory and the display.
    let firmware_range = |tag| {
        let mut value = [0u32; 2];
        mailbox
            .property(tag, &mut value)
            .map(|()| (PhysAddr::new(value[0] as usize), value[1] as usize))
    };
    let arm_memory = firmware_range(mailbox::TAG_GET_ARM_MEMORY);
    let vc_memory = firmware_range(mailbox::TAG_GET_VC_MEMORY);
    let displays = framebuffer::count_displays(mailbox);
    let framebuffer = framebuffer::FrameBuffer::allocate(mailbox);
    let device_tree = device_tree(PhysAddr::new(dtb));

    // Physical memory and the kernel's own memory map; then the heap.
    let boot = memory::BootInfo {
        fdt: device_tree,
        fdt_range: device_tree.map(|fdt| (PhysAddr::new(dtb), fdt.size())),
        arm_memory,
        vc_memory,
        framebuffer: framebuffer.map(|(fb, _)| fb.memory()),
        devices: model.devices(),
    };
    let memory_report = match memory::init(&boot) {
        Ok(report) => report,
        Err(e) => panic!("memory setup failed: {e}"),
    };
    let heap_region = heap::init();
    let display = framebuffer.and_then(|(fb, _)| framebuffer::start(fb, displays));

    // Interrupts: the timer tick and interrupt-driven serial input.
    let (gicd, gicc) = model.gic();
    let irq_lines = irq::init(gicd, gicc);
    irq::register(timer::TICK_IRQ, on_tick);
    timer::start_tick();
    console::enable_interrupts();
    irq::enable();

    // From here on this is a thread like any other, taking turns with the
    // rest.
    thread::init("kernel");

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
    match &device_tree {
        Some(fdt) => println!("  device tree     : {:#010x} ({} bytes)", dtb, fdt.size()),
        None => println!("  device tree     : {:#010x} (not valid)", dtb),
    }
    let (image_start, image_end) = mmu::kernel_image();
    println!(
        "  kernel image    : {:#010x} - {:#010x}, at {:#x}",
        image_start, image_end, &raw const __kernel_start as usize
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
    let frames = memory::frame_stats();
    println!(
        "  RAM             : {} MiB in {} range(s), from the {}; {} of {} MiB free in 4 KiB pages",
        memory_report.ram.total() >> 20,
        memory_report.ram.len(),
        if memory_report.from_device_tree {
            "device tree"
        } else {
            "firmware"
        },
        (frames.free * addr::PAGE_SIZE) >> 20,
        (frames.total * addr::PAGE_SIZE) >> 20
    );
    println!("  memory map      : kernel in the upper half, code read-only, data never executable");
    println!(
        "  interrupts      : GIC-400 at {:#x} ({} IDs), {} Hz timer tick",
        gicd,
        irq_lines,
        timer::TICK_HZ
    );
    match heap_region {
        Some((start, size)) => println!("  heap            : {} MiB at {:#x}", size >> 20, start),
        None => println!("  heap            : none (not enough free memory)"),
    }
    let boot_image = bootimage::image();
    print!(
        "  boot image      : {} KiB, {} programs:",
        boot_image.as_bytes().len() >> 10,
        boot_image.len()
    );
    for file in boot_image.files() {
        print!(" {}", file.name);
    }
    println!();
    let threads = thread::stats();
    println!(
        "  threads         : {}, round-robin with {} ms time slices; {} switches so far, {} preemptive",
        threads.threads,
        1000 / timer::TICK_HZ,
        threads.switches,
        threads.preemptions
    );
    match self_test {
        Ok(()) => println!(
            "  self-test       : svc, brk, timer interrupts, MMU, atomics, heap, address spaces, user mode, threads and IPC OK"
        ),
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
    if let Some((start, size)) = arm_memory {
        println!(
            "  ARM memory      : {:#010x} - {:#010x} ({} MiB)",
            start,
            start + size,
            size >> 20
        );
    }

    // Start the system: `init`, from the boot image, starts the rest.
    println!();
    match process::spawn_from_boot_image("init").map(thread::JoinHandle::join) {
        Ok(process::Exit::Code(code)) => println!("[init exited with code {}]", code),
        Ok(process::Exit::Fault(fault)) => println!("[init was stopped: {}]", fault),
        Err(e) => println!("[init could not be started: {}]", e),
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

    // With the MMU on, RAM is Normal memory, where unaligned accesses work
    // (as Device memory, with the MMU off, they fault) and so do atomic
    // read-modify-write instructions. The compiler never emits unaligned
    // accesses for this target, so use one directly.
    if !mmu::enabled() {
        return Err("the MMU or caches are off");
    }
    let words = [0x1122_3344_5566_7788u64, 0x99AA_BBCC_DDEE_FF00];
    let unaligned: u64;
    // SAFETY: bytes 3..11 lie within `words`.
    unsafe {
        core::arch::asm!("ldr {}, [{}]", out(reg) unaligned, in(reg) (words.as_ptr() as *const u8).add(3),
                         options(readonly, nostack))
    };
    if unaligned != 0xEEFF_0011_2233_4455 {
        return Err("unaligned load from RAM returned the wrong value");
    }
    let counter = core::sync::atomic::AtomicU32::new(1);
    let old = core::hint::black_box(&counter).fetch_add(1, core::sync::atomic::Ordering::SeqCst);
    if old != 1 || counter.into_inner() != 2 {
        return Err("atomic read-modify-write failed");
    }

    heap::self_test()?;
    addrspace::self_test()?;
    process::self_test()?;
    Ok(())
}

/// The timer interrupt: count the tick, and end the running thread's time
/// slice.
fn on_tick() {
    timer::handle_tick();
    thread::tick();
}

/// The device tree the firmware left at `phys`, if there is a valid one
/// within the boot map (the first GiB of RAM).
fn device_tree(phys: PhysAddr) -> Option<fdt::Fdt<'static>> {
    const BOOT_MAP_END: usize = 1 << 30;
    let p = phys.as_usize();
    if p == 0 || !p.is_multiple_of(8) || p + 8 > BOOT_MAP_END {
        return None;
    }
    // SAFETY: the first 8 bytes are within the boot map's RAM.
    let header = unsafe { core::slice::from_raw_parts(phys.to_virt().as_ptr::<u8>(), 8) };
    let size = fdt::Fdt::total_size(header).ok()?;
    if size > 16 << 20 || p + size > BOOT_MAP_END {
        return None;
    }
    // SAFETY: the whole device tree is within the boot map's RAM, which
    // stays mapped (memory::init keeps it out of the frame allocator), and
    // nothing writes to it.
    let bytes = unsafe { core::slice::from_raw_parts(phys.to_virt().as_ptr::<u8>(), size) };
    fdt::Fdt::new(bytes).ok()
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
