//! System calls: the kernel side of the interface in the `pios-abi` crate.
//!
//! Handles are looked up in the calling process's table, and each call
//! checks the handle has the right it needs. Taking a handle out of the
//! table and dropping it (which may wake other threads) happen separately,
//! so no handle is dropped with the table locked.

use alloc::sync::Arc;
use alloc::vec::Vec;

use pios_abi::{Error, ExitStatus, MESSAGE_SIZE, Message, call, rights};

use crate::device::Timer;
use crate::exception::TrapFrame;
use crate::handle::Handle;
use crate::ipc::{self, Endpoint, EndpointRef, ReplyCap};
use crate::paging::MapError;
use crate::process::{self, Exit, LoadError, Process};
use crate::user::{UserSlice, UserSliceMut};

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
        call::SPAWN => spawn(args[0], args[1], args[2], args[3]),
        call::WAIT => wait(args[0]),
        call::CLOSE => close(args[0]),
        call::ENDPOINT => endpoint(),
        call::DUPLICATE => duplicate(args[0], args[1], args[2] as u64),
        call::SEND => send(args[0], args[1], false),
        call::CALL => send(args[0], args[1], true),
        call::RECEIVE => receive(args[0], args[1]),
        call::REPLY => reply(args[0], args[1]),
        call::MAP => map(args[0], args[1]),
        call::INTERRUPT_BIND => interrupt_bind(args[0], args[1], args[2] as u64),
        call::INTERRUPT_ACK => interrupt_ack(args[0]),
        call::TIMER => timer(args[0], args[1] as u64, args[2] as u64),
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
fn caller() -> Arc<Process> {
    crate::thread::current_process().expect("system calls come from user processes")
}

/// Add `handle` to the caller's table.
fn insert(process: &Process, handle: Handle) -> Result<usize, Error> {
    let result = process.handles().lock().insert(handle);
    // (A handle that didn't fit is dropped here, without the lock.)
    result.map_err(|_| Error::OutOfMemory)
}

/// Take handle `number` out of the caller's table to give it away: it needs
/// the `TRANSFER` right. 0 means no handle.
fn take_for_transfer(process: &Process, number: usize) -> Result<Option<Handle>, Error> {
    if number == 0 {
        return Ok(None);
    }
    let taken = process.handles().lock().remove_if(number, |handle| {
        if handle.has(rights::TRANSFER) {
            Ok(())
        } else {
            Err(Error::AccessDenied)
        }
    });
    match taken {
        Some(result) => result.map(Some),
        None => Err(Error::BadHandle),
    }
}

/// The endpoint handle `number` refers to, and its badge, if the handle has
/// `right`.
fn endpoint_with(
    process: &Process,
    number: usize,
    right: usize,
) -> Result<(Arc<Endpoint>, u64), Error> {
    match process.handles().lock().get(number) {
        Some(Handle::Endpoint(e)) if e.rights() & right == right => {
            Ok((e.endpoint().clone(), e.badge()))
        }
        Some(Handle::Endpoint(_)) => Err(Error::AccessDenied),
        _ => Err(Error::BadHandle),
    }
}

/// `spawn(ptr, len, arg, handle)`: start the executable in the caller's
/// memory as a new process, and give the caller a handle to it. If the
/// process can't be started, a handle passed to it is closed.
fn spawn(addr: usize, len: usize, arg: usize, handle: usize) -> Result<usize, Error> {
    if len > pios_abi::SPAWN_MAX {
        return Err(Error::InvalidArgument);
    }
    let source = UserSlice::new(addr, len)?;
    let caller = caller();
    if !caller.handles().lock().has_room(1) {
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
    let handle = take_for_transfer(&caller, handle)?;
    let child = process::spawn("user", &image, arg, handle).map_err(|e| match e {
        LoadError::Map(MapError::OutOfMemory) | LoadError::NoThread => Error::OutOfMemory,
        _ => Error::InvalidArgument,
    })?;
    // (Only this process's own thread adds handles, and it is here, so the
    // table still has room.)
    insert(&caller, Handle::Process(child))
}

/// `wait(handle)`: wait for a process the caller started to end, and close
/// the handle.
fn wait(number: usize) -> Result<usize, Error> {
    let caller = caller();
    let taken = caller
        .handles()
        .lock()
        .remove_if(number, |handle| match handle {
            Handle::Process(_) => Ok(()),
            _ => Err(Error::BadHandle),
        });
    let child = match taken {
        Some(Ok(Handle::Process(child))) => child,
        Some(Err(error)) => return Err(error),
        _ => return Err(Error::BadHandle),
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
fn close(number: usize) -> Result<usize, Error> {
    let removed = caller().handles().lock().remove(number);
    match removed {
        Some(_) => Ok(0),
        None => Err(Error::BadHandle),
    }
}

/// `endpoint()`: a new endpoint, with every right.
fn endpoint() -> Result<usize, Error> {
    let handle = EndpointRef::new(Endpoint::new(), rights::ENDPOINT_ALL, 0);
    insert(&caller(), Handle::Endpoint(handle))
}

/// `duplicate(handle, rights, badge)`: a copy of an endpoint handle with no
/// more rights, perhaps with a badge.
fn duplicate(number: usize, new_rights: usize, badge: u64) -> Result<usize, Error> {
    let caller = caller();
    let (endpoint, badge) = match caller.handles().lock().get(number) {
        None => return Err(Error::BadHandle),
        Some(handle) if !handle.has(rights::DUPLICATE) => return Err(Error::AccessDenied),
        Some(Handle::Endpoint(e)) => {
            if new_rights & !e.rights() != 0 {
                return Err(Error::AccessDenied);
            }
            // A badge can be set once, on a handle that hasn't one.
            if badge != 0 && e.badge() != 0 {
                return Err(Error::InvalidArgument);
            }
            let badge = if badge == 0 { e.badge() } else { badge };
            (e.endpoint().clone(), badge)
        }
        Some(_) => return Err(Error::AccessDenied),
    };
    insert(
        &caller,
        Handle::Endpoint(EndpointRef::new(endpoint, new_rights, badge)),
    )
}

fn read_message(addr: usize) -> Result<Message, Error> {
    let mut bytes = [0; MESSAGE_SIZE];
    UserSlice::new(addr, MESSAGE_SIZE)?.read(0, &mut bytes);
    Ok(Message::from_bytes(&bytes))
}

/// A message from the caller's memory, in kernel form, taking the handle it
/// carries out of the caller's table.
fn take_message(process: &Process, message: &Message) -> Result<ipc::Message, Error> {
    Ok(ipc::Message {
        label: message.label,
        data: message.data,
        handle: take_for_transfer(process, message.handle as usize)?,
    })
}

/// Hand a message to the caller: install its handles in the caller's table
/// (which the caller checked has room) and write it to `out`.
fn deliver(
    process: &Process,
    message: ipc::Message,
    badge: u64,
    reply: Option<ReplyCap>,
    out: &UserSliceMut,
) {
    let mut install = |handle| {
        insert(process, handle).unwrap_or_else(|_| unreachable!("room was checked")) as u64
    };
    let message = Message {
        label: message.label,
        handle: message.handle.map_or(0, &mut install),
        badge,
        reply: reply.map_or(0, |reply| install(Handle::Reply(reply))),
        data: message.data,
    };
    out.write(0, &message.to_bytes());
}

/// `send(endpoint, message)` and `call(endpoint, message)`.
fn send(number: usize, addr: usize, call: bool) -> Result<usize, Error> {
    let caller = caller();
    let message = read_message(addr)?;
    // Check everything that could fail before the message's handle is
    // taken, so a mistake doesn't lose it.
    let out = if call {
        if !caller.handles().lock().has_room(1) {
            return Err(Error::OutOfMemory);
        }
        Some(UserSliceMut::new(addr, MESSAGE_SIZE)?)
    } else {
        None
    };
    let (endpoint, badge) = endpoint_with(&caller, number, rights::SEND)?;
    let message = take_message(&caller, &message)?;
    if let Some(reply) = endpoint.send(message, badge, call)? {
        let out = out.expect("only calls are replied to");
        deliver(&caller, reply, 0, None, &out);
    }
    Ok(0)
}

/// `receive(endpoint, message)`.
fn receive(number: usize, addr: usize) -> Result<usize, Error> {
    let caller = caller();
    let out = UserSliceMut::new(addr, MESSAGE_SIZE)?;
    // Room for a handle in the message and a reply handle.
    if !caller.handles().lock().has_room(2) {
        return Err(Error::OutOfMemory);
    }
    let (endpoint, _) = endpoint_with(&caller, number, rights::RECEIVE)?;
    let delivery = endpoint.receive()?;
    deliver(
        &caller,
        delivery.message,
        delivery.badge,
        delivery.reply,
        &out,
    );
    Ok(0)
}

/// `reply(reply, message)`: answer a call, using up its reply handle.
fn reply(number: usize, addr: usize) -> Result<usize, Error> {
    let caller = caller();
    let message = read_message(addr)?;
    if !matches!(caller.handles().lock().get(number), Some(Handle::Reply(_))) {
        return Err(Error::BadHandle);
    }
    if message.handle as usize == number {
        return Err(Error::InvalidArgument);
    }
    let message = take_message(&caller, &message)?;
    let removed = caller.handles().lock().remove(number);
    match removed {
        Some(Handle::Reply(reply)) => reply.reply(message),
        _ => unreachable!("checked above, and only this thread changes the table"),
    }
    Ok(0)
}

/// `map(memory, address)`: map memory the caller holds a handle to.
fn map(number: usize, va: usize) -> Result<usize, Error> {
    let caller = caller();
    let memory = match caller.handles().lock().get(number) {
        Some(Handle::Memory(memory)) => *memory,
        Some(_) => return Err(Error::BadHandle),
        None => return Err(Error::BadHandle),
    };
    let result = caller.space().lock().map_memory(va, &memory);
    match result {
        Ok(()) => Ok(0),
        Err(MapError::OutOfMemory) => Err(Error::OutOfMemory),
        Err(_) => Err(Error::InvalidArgument),
    }
}

/// A new reference to endpoint handle `number` for the kernel to notify
/// through: the handle needs `SEND`.
fn notifier(process: &Process, number: usize) -> Result<EndpointRef, Error> {
    let (endpoint, _) = endpoint_with(process, number, rights::SEND)?;
    Ok(EndpointRef::new(endpoint, rights::SEND, 0))
}

/// `interrupt_bind(interrupt, endpoint, badge)`.
fn interrupt_bind(number: usize, endpoint: usize, badge: u64) -> Result<usize, Error> {
    let caller = caller();
    if !matches!(
        caller.handles().lock().get(number),
        Some(Handle::Interrupt(_))
    ) {
        return Err(Error::BadHandle);
    }
    if badge == 0 {
        return Err(Error::InvalidArgument);
    }
    let endpoint = notifier(&caller, endpoint)?;
    match caller.handles().lock().get(number) {
        Some(Handle::Interrupt(interrupt)) => interrupt.bind(endpoint, badge),
        _ => unreachable!("checked above, and only this thread changes the table"),
    }
    Ok(0)
}

/// `interrupt_ack(interrupt)`.
fn interrupt_ack(number: usize) -> Result<usize, Error> {
    match caller().handles().lock().get(number) {
        Some(Handle::Interrupt(interrupt)) => {
            interrupt.acknowledge();
            Ok(0)
        }
        _ => Err(Error::BadHandle),
    }
}

/// `timer(endpoint, badge, period_ms)`.
fn timer(endpoint: usize, badge: u64, period_ms: u64) -> Result<usize, Error> {
    let caller = caller();
    if badge == 0 || period_ms == 0 {
        return Err(Error::InvalidArgument);
    }
    if !caller.handles().lock().has_room(1) {
        return Err(Error::OutOfMemory);
    }
    let endpoint = notifier(&caller, endpoint)?;
    insert(
        &caller,
        Handle::Timer(Timer::start(endpoint, badge, period_ms)),
    )
}
