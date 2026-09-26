//! System calls: the kernel side of the interface in the `pios-abi` crate.

use alloc::vec::Vec;

use pios_abi::{Error, ExitStatus, call};

use crate::exception::TrapFrame;
use crate::handle::Object;
use crate::paging::MapError;
use crate::process::{self, Exit, LoadError, Process};
use crate::user::UserSlice;

/// Handle a system call from a user program: the call number is in x8,
/// the arguments in x0-x5, and the result goes back in x0.
pub fn handle(frame: &mut TrapFrame) {
    let args = [
        frame.x[0], frame.x[1], frame.x[2], frame.x[3], frame.x[4], frame.x[5],
    ]
    .map(|a| a as usize);
    let result = match frame.x[8] as usize {
        call::DEBUG_WRITE => debug_write(args[0], args[1]),
        call::EXIT => crate::process::exit(args[0] as i32),
        call::YIELD => {
            crate::thread::yield_now();
            Ok(0)
        }
        call::SPAWN => spawn(args[0], args[1], args[2]),
        call::WAIT => wait(args[0]),
        call::CLOSE => close(args[0]),
        _ => Err(Error::NoSuchCall),
    };
    frame.x[0] = match result {
        Ok(value) => value as u64,
        Err(error) => error.to_raw() as u64,
    };
}

/// `debug_write(ptr, len)`: write the program's bytes to the kernel
/// console.
fn debug_write(addr: usize, len: usize) -> Result<usize, Error> {
    if len > pios_abi::DEBUG_WRITE_MAX {
        return Err(Error::InvalidArgument);
    }
    let text = UserSlice::new(addr, len)?;
    let mut buffer = [0u8; 256];
    let mut done = 0;
    while done < text.len() {
        let chunk = &mut buffer[..(text.len() - done).min(256)];
        text.read(done, chunk);
        crate::console::write_bytes(chunk);
        done += chunk.len();
    }
    Ok(len)
}

/// The process making the system call.
fn caller() -> alloc::sync::Arc<Process> {
    crate::thread::current_process().expect("system calls come from user processes")
}

/// `spawn(ptr, len, arg)`: start the executable in the caller's memory as a
/// new process, and give the caller a handle to it.
fn spawn(addr: usize, len: usize, arg: usize) -> Result<usize, Error> {
    if len > pios_abi::SPAWN_MAX {
        return Err(Error::InvalidArgument);
    }
    let source = UserSlice::new(addr, len)?;
    let caller = caller();
    if caller.handles().lock().is_full() {
        return Err(Error::OutOfMemory);
    }
    // Copy the executable in first: it is only checked once, so it mustn't
    // change underneath the loader.
    let mut image = Vec::new();
    image
        .try_reserve_exact(len)
        .map_err(|_| Error::OutOfMemory)?;
    image.resize(len, 0);
    source.read(0, &mut image);
    let child = process::spawn("user", &image, arg).map_err(|e| match e {
        LoadError::Map(MapError::OutOfMemory) | LoadError::NoThread => Error::OutOfMemory,
        _ => Error::InvalidArgument,
    })?;
    let handle = caller.handles().lock().insert(Object::Process(child));
    // (Only this process's own thread adds handles, and it is here, so the
    // table still has room.)
    Ok(handle.unwrap_or_else(|_| unreachable!("the handle table filled up")))
}

/// `wait(handle)`: wait for a process the caller started to end, and close
/// the handle.
fn wait(handle: usize) -> Result<usize, Error> {
    let caller = caller();
    let object = {
        let mut handles = caller.handles().lock();
        match handles.get(handle) {
            Some(Object::Process(_)) => handles.remove(handle),
            None => None,
        }
    };
    let Some(Object::Process(child)) = object else {
        return Err(Error::BadHandle);
    };
    let status = match child.join() {
        Exit::Code(code) => ExitStatus::Code(code),
        Exit::Fault(fault) => ExitStatus::Fault {
            esr: fault.esr as u32,
        },
    };
    Ok(status.to_raw())
}

/// `close(handle)`: give up a handle.
fn close(handle: usize) -> Result<usize, Error> {
    match caller().handles().lock().remove(handle) {
        Some(_) => Ok(0),
        None => Err(Error::BadHandle),
    }
}
