//! The first program. The kernel starts it with handles to the hardware it
//! used for its console and to the boot image (described by the `BootInfo`
//! at the address in its argument). It starts the console server and hands
//! it that hardware, starts the process manager and hands it the boot image,
//! then keeps a shell running.

#![no_std]
#![no_main]

use libpios::console::{self, CLIENT_BADGE, SETUP_BADGE};
use libpios::procman;
use libpios::rights::{DUPLICATE, RECEIVE, SEND, TRANSFER};
use libpios::{ExitStatus, Handle, Message, println};
use pios_abi::{BOOT_INFO_MAGIC, BootInfo};
use pios_bootfs::BootFs;

libpios::pios_main!(main);

/// What the kernel says about the machine.
fn boot_info() -> Option<&'static BootInfo> {
    // SAFETY: the kernel maps a BootInfo read-only at our argument for as
    // long as we run; we check its magic number before trusting the rest.
    let info = unsafe { &*(libpios::argument() as *const BootInfo) };
    (info.magic == BOOT_INFO_MAGIC).then_some(info)
}

/// The boot image the kernel mapped for us.
fn boot_image(info: &BootInfo) -> Option<BootFs<'static>> {
    // SAFETY: the kernel maps the boot image read-only where BootInfo says,
    // for as long as we run.
    let image = unsafe {
        core::slice::from_raw_parts(info.boot_image as *const u8, info.boot_image_size as usize)
    };
    BootFs::new(image).ok()
}

/// Hand one piece of hardware to the console server.
fn give(setup: &Handle, label: u64, handle: u64, data: &[u64]) -> bool {
    let message = Message::new(label, data).with_handle(Handle::from_raw(handle as usize));
    matches!(setup.call(message), Ok(reply) if reply.label == console::OK)
}

/// Start the console server, set it up with the hardware in `info`, and
/// return the endpoint to hand out client handles from.
fn start_console(image: &BootFs, info: &BootInfo) -> Option<Handle> {
    let endpoint = libpios::endpoint().ok()?;
    let server = endpoint.duplicate(SEND | RECEIVE | TRANSFER, 0).ok()?;
    let program = image.find("console")?.data;
    // The server runs for good: dropping the child handle doesn't stop it.
    drop(libpios::spawn(program, 0, Some(server), "").ok()?);

    let setup = endpoint.duplicate(SEND, SETUP_BADGE).ok()?;
    let mut ok = true;
    for (i, uart) in info.uarts.iter().enumerate() {
        if uart.memory != 0 {
            ok &= give(&setup, console::SETUP_UART, uart.memory, &[i as u64]);
            if uart.interrupt != 0 {
                ok &= give(
                    &setup,
                    console::SETUP_UART_INTERRUPT,
                    uart.interrupt,
                    &[i as u64],
                );
            }
        }
    }
    let d = &info.display;
    if d.memory != 0 {
        ok &= give(
            &setup,
            console::SETUP_DISPLAY,
            d.memory,
            &[d.width, d.height, d.pitch, d.size],
        );
        ok &= give(
            &setup,
            console::SETUP_DISPLAY_TEXT,
            d.text,
            &[d.text_size, d.columns, d.rows, d.cursor_x, d.cursor_y],
        );
    }
    let done = setup.call(Message::new(console::SETUP_DONE, &[]));
    ok &= matches!(done, Ok(reply) if reply.label == console::OK);
    ok.then_some(endpoint)
}

/// Start the process manager and set it up with the boot image and a
/// console handle to give the programs it starts. Returns the endpoint to
/// hand out client handles from.
fn start_procman(image: &BootFs, info: &BootInfo, console: &Handle) -> Option<Handle> {
    let endpoint = libpios::endpoint().ok()?;
    let server = endpoint.duplicate(SEND | RECEIVE | TRANSFER, 0).ok()?;
    let program = image.find("procman")?.data;
    drop(libpios::spawn(program, 0, Some(server), "").ok()?);

    let setup = endpoint.duplicate(SEND, procman::SETUP_BADGE).ok()?;
    let consoles = console
        .duplicate(SEND | DUPLICATE | TRANSFER, CLIENT_BADGE)
        .ok()?;
    let ok = give(
        &setup,
        procman::SETUP_BOOT_IMAGE,
        info.boot_image_memory,
        &[info.boot_image_size],
    ) && give(
        &setup,
        procman::SETUP_CONSOLE,
        consoles.into_raw() as u64,
        &[],
    );
    let done = setup.call(Message::new(procman::SETUP_DONE, &[]));
    (ok && matches!(done, Ok(reply) if reply.label == procman::OK)).then_some(endpoint)
}

fn main() -> i32 {
    let Some(info) = boot_info() else {
        println!("init: no boot information");
        return 1;
    };
    let Some(image) = boot_image(info) else {
        println!("init: no valid boot image");
        return 1;
    };
    let Some(console) = start_console(&image, info) else {
        println!("init: the console server didn't start");
        return 1;
    };
    if let Ok(mine) = console.duplicate(SEND, CLIENT_BADGE) {
        console::connect(mine);
    }
    println!(
        "init: starting the system from a boot image of {} programs",
        image.len()
    );
    let uarts = info.uarts.iter().filter(|u| u.memory != 0).count();
    let display = if info.display.memory != 0 {
        " and the display"
    } else {
        ""
    };
    println!(
        "init: the console server has {} serial port(s){}",
        uarts, display
    );
    let Some(procman) = start_procman(&image, info, &console) else {
        println!("init: the process manager didn't start");
        return 1;
    };
    let Some(shell) = image.find("shell") else {
        println!("init: there is no shell");
        return 1;
    };

    // Keep a shell running: start a new one whenever it ends.
    loop {
        let handle = procman
            .duplicate(SEND | TRANSFER, procman::CLIENT_BADGE)
            .ok();
        match libpios::spawn(shell.data, 0, handle, "").and_then(|child| child.wait()) {
            Ok(ExitStatus::Code(0)) => println!("init: the shell exited; starting a new one"),
            Ok(status) => println!("init: the shell ended ({:?}); starting a new one", status),
            Err(error) => {
                println!("init: couldn't start the shell: {:?}", error);
                return 1;
            }
        }
    }
}
