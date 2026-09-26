//! The first program: the kernel starts it with the boot image mapped
//! read-only at the address in its argument, and it starts everything else
//! from there. For now that is just `hello`.

#![no_std]
#![no_main]

use libpios::{ExitStatus, println};
use pios_bootfs::{BootFs, HEADER_SIZE};

libpios::pios_main!(main);

/// The boot image the kernel mapped for us at `addr`.
fn boot_image(addr: usize) -> Option<BootFs<'static>> {
    // SAFETY: the kernel maps the whole boot image read-only at `addr` for
    // as long as we run, starting with its header.
    let header = unsafe { core::slice::from_raw_parts(addr as *const u8, HEADER_SIZE) };
    let size = BootFs::total_size(header).ok()?;
    // SAFETY: as above; the header gives the image's size.
    let image = unsafe { core::slice::from_raw_parts(addr as *const u8, size) };
    BootFs::new(image).ok()
}

/// Start `name` from the boot image and wait for it, reporting how it
/// ended.
fn run(image: &BootFs, name: &str) -> bool {
    let Some(file) = image.find(name) else {
        println!("init: {} is not in the boot image", name);
        return false;
    };
    match libpios::spawn(file.data, 0, None).and_then(|child| child.wait()) {
        Ok(ExitStatus::Code(code)) => {
            println!("[{} exited with code {}]", name, code);
            code == 0
        }
        Ok(ExitStatus::Fault { esr }) => {
            println!("[{} was stopped by a fault, syndrome {:#x}]", name, esr);
            false
        }
        Err(e) => {
            println!("init: could not start {}: {:?}", name, e);
            false
        }
    }
}

fn main() -> i32 {
    let Some(image) = boot_image(libpios::argument()) else {
        println!("init: no valid boot image");
        return 1;
    };
    println!(
        "init: starting the system from a boot image of {} programs",
        image.len()
    );
    if run(&image, "hello") { 0 } else { 1 }
}
