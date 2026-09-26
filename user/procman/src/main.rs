//! The process manager: starts programs from the boot image by name.
//!
//! `init` starts it with its endpoint and sets it up with the boot image and
//! a console handle it can make copies of (see `libpios::procman` for the
//! protocol). Each program it starts gets a console handle as its start
//! handle and the rest of its command line as its arguments, and the
//! caller gets the new process's handle, to wait for it.
//!
//! Programs that only work when started some other way aren't offered: the
//! parts of the system itself (started by `init`, with handles these
//! wouldn't get) and the kernel's IPC self-test (started by the kernel,
//! with the boot image).

#![no_std]
#![no_main]

use libpios::console::{MAX_BYTES, pack, unpack};
use libpios::procman::{self, FAILED, NOT_FOUND, OK, REFUSED};
use libpios::rights::{SEND, TRANSFER};
use libpios::{Handle, Message, println};
use pios_bootfs::BootFs;

libpios::pios_main!(main);

/// Where the boot image is mapped.
const BOOT_IMAGE_ADDR: usize = 0x30_0000_0000;

/// Programs not for running by name.
const SYSTEM: [&str; 5] = ["init", "console", "procman", "shell", "ipctest"];

/// The programs that can be run, in name order.
fn programs(image: &BootFs<'static>) -> impl Iterator<Item = pios_bootfs::File<'static>> {
    image.files().filter(|file| !SYSTEM.contains(&file.name))
}

struct Setup {
    image: Option<BootFs<'static>>,
    console: Option<Handle>,
}

impl Setup {
    /// Handle one set-up message from init. Returns whether set-up is done.
    fn handle(&mut self, message: Message) -> Result<bool, ()> {
        match message.label {
            procman::SETUP_BOOT_IMAGE => {
                message
                    .handle
                    .ok_or(())?
                    .map(BOOT_IMAGE_ADDR)
                    .map_err(|_| ())?;
                let size = message.data[0] as usize;
                // SAFETY: the boot image is now mapped read-only there for
                // good; its size we take from init, which got it from the
                // kernel with the handle.
                let bytes =
                    unsafe { core::slice::from_raw_parts(BOOT_IMAGE_ADDR as *const u8, size) };
                self.image = Some(BootFs::new(bytes).map_err(|_| ())?);
            }
            procman::SETUP_CONSOLE => self.console = Some(message.handle.ok_or(())?),
            procman::SETUP_DONE if self.image.is_some() && self.console.is_some() => {
                return Ok(true);
            }
            _ => return Err(()),
        }
        Ok(false)
    }
}

fn run(image: &BootFs<'static>, console: &Handle, message: &Message) -> Message {
    let mut line = [0u8; MAX_BYTES];
    let len = unpack(message, &mut line);
    let Ok(line) = core::str::from_utf8(&line[..len]) else {
        return Message::new(REFUSED, &[]);
    };
    let line = line.trim();
    let (name, args) = line.split_once(' ').unwrap_or((line, ""));
    let Some(program) = programs(image).find(|file| file.name == name) else {
        return Message::new(NOT_FOUND, &[]);
    };
    let started = console
        .duplicate(SEND | TRANSFER, 0)
        .and_then(|handle| libpios::spawn(program.data, 0, Some(handle), args.trim_start()));
    match started {
        Ok(child) => Message::new(OK, &[]).with_handle(child.into_handle()),
        Err(error) => Message::new(FAILED, &[error.to_raw() as u64]),
    }
}

fn main() -> i32 {
    let Some(endpoint) = libpios::start_handle() else {
        println!("procman: started without an endpoint");
        return 1;
    };

    let mut setup = Setup {
        image: None,
        console: None,
    };
    loop {
        let Ok(request) = endpoint.receive() else {
            return 1;
        };
        let result = if request.badge == procman::SETUP_BADGE {
            setup.handle(request.message)
        } else {
            Err(())
        };
        if let Some(reply) = request.reply {
            let label = if result.is_ok() { OK } else { REFUSED };
            let _ = reply.reply(Message::new(label, &[]));
        }
        if result == Ok(true) {
            break;
        }
    }
    let (Some(image), Some(console)) = (setup.image, setup.console) else {
        return 1;
    };

    loop {
        let Ok(request) = endpoint.receive() else {
            return 1;
        };
        let Some(reply) = request.reply else { continue };
        let message = request.message;
        let answer = match (request.badge, message.label) {
            (procman::CLIENT_BADGE, procman::RUN) => run(&image, &console, &message),
            (procman::CLIENT_BADGE, procman::LIST) => {
                match programs(&image).nth(message.data[0] as usize) {
                    Some(file) => pack(OK, &file.name.as_bytes()[..file.name.len().min(MAX_BYTES)]),
                    None => Message::new(NOT_FOUND, &[]),
                }
            }
            (procman::CLIENT_BADGE, procman::CONSOLE) => {
                match console.duplicate(SEND | TRANSFER, 0) {
                    Ok(handle) => Message::new(OK, &[]).with_handle(handle),
                    Err(error) => Message::new(FAILED, &[error.to_raw() as u64]),
                }
            }
            _ => Message::new(REFUSED, &[]),
        };
        let _ = reply.reply(answer);
    }
}
