//! The process manager's protocol, and the client side of it.
//!
//! The process manager starts programs by name from the boot image, giving
//! each a handle to the console as its start handle, and hands back the new
//! process's handle so the caller can wait for it. Names and command lines
//! travel as text, as in [`console`](crate::console).
//!
//! | Label | Request | Reply |
//! | --- | --- | --- |
//! | `RUN` | command line: program name, then its arguments | `OK` with the process handle; `NOT_FOUND`; or `FAILED` with the error |
//! | `LIST` | index | `OK` with the name of the program at that index, or `NOT_FOUND` past the end |
//! | `CONSOLE` | | `OK` with a handle to the console |
//!
//! `init` sets it up first, through a handle with `SETUP_BADGE`:
//! `SETUP_BOOT_IMAGE` (a memory handle to the boot image; data: its size),
//! `SETUP_CONSOLE` (a console handle it can duplicate), then `SETUP_DONE`.

use pios_abi::Error;

use crate::console::{MAX_BYTES, pack, unpack};
use crate::{Child, Handle, Message};

pub const RUN: u64 = 1;
pub const LIST: u64 = 2;
pub const CONSOLE: u64 = 3;
pub const SETUP_BOOT_IMAGE: u64 = 100;
pub const SETUP_CONSOLE: u64 = 101;
pub const SETUP_DONE: u64 = 102;

/// Reply labels.
pub const OK: u64 = 0;
pub const NOT_FOUND: u64 = 1;
pub const REFUSED: u64 = 2;
pub const FAILED: u64 = 3;

pub const SETUP_BADGE: u64 = 1;
pub const CLIENT_BADGE: u64 = 2;

/// Why `run` failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunError {
    /// No program by that name.
    NotFound,
    /// The command line doesn't fit in a message.
    TooLong,
    /// The process manager couldn't start it, or couldn't be reached.
    Failed(Error),
}

/// Ask the process manager (through `procman`) to run `command_line`: the
/// program's name, then its arguments.
pub fn run(procman: &Handle, command_line: &str) -> Result<Child, RunError> {
    if command_line.len() > MAX_BYTES {
        return Err(RunError::TooLong);
    }
    let reply = procman
        .call(pack(RUN, command_line.as_bytes()))
        .map_err(RunError::Failed)?;
    match (reply.label, reply.handle) {
        (OK, Some(process)) => Ok(Child::from_handle(process)),
        (NOT_FOUND, _) => Err(RunError::NotFound),
        (FAILED, _) => Err(RunError::Failed(
            Error::from_result(reply.data[0] as usize)
                .err()
                .unwrap_or(Error::InvalidArgument),
        )),
        _ => Err(RunError::Failed(Error::InvalidArgument)),
    }
}

/// The name of the program at `index` in the boot image, copied to `name`;
/// returns its length, or `None` past the end.
pub fn program(procman: &Handle, index: usize, name: &mut [u8]) -> Option<usize> {
    let reply = procman.call(Message::new(LIST, &[index as u64])).ok()?;
    (reply.label == OK).then(|| unpack(&reply, name))
}

/// A handle to the console, for a program that wasn't started with one.
pub fn console(procman: &Handle) -> Result<Handle, Error> {
    let reply = procman.call(Message::new(CONSOLE, &[]))?;
    match (reply.label, reply.handle) {
        (OK, Some(console)) => Ok(console),
        _ => Err(Error::InvalidArgument),
    }
}
