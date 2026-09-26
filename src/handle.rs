//! Handle tables: each process's numbered references to kernel objects.
//!
//! A system call names an object by its handle, and holding the handle,
//! with the right rights, is the permission to use it. The kernel owns the
//! table, so handles can't be forged, and rights can only be dropped, never
//! added (see docs/design.md).

use alloc::collections::BTreeMap;

use pios_abi::rights;

use crate::device::{Interrupt, MEMORY_RIGHTS, Memory, Timer};
use crate::ipc::{EndpointRef, ReplyCap};
use crate::thread::JoinHandle;

/// The most handles one process may hold.
pub const MAX_HANDLES: usize = 1024;

/// What a handle refers to.
pub enum Handle {
    /// A process the holder started: it can wait for it to end.
    Process(JoinHandle),
    /// An IPC endpoint, with the handle's rights and badge.
    Endpoint(EndpointRef),
    /// The way to answer one call.
    Reply(ReplyCap),
    /// Memory the holder may map.
    Memory(Memory),
    /// Ownership of an interrupt.
    Interrupt(Interrupt),
    /// A periodic notification; closing the handle stops it (which is all
    /// the handle is for: holding it keeps the timer going).
    Timer(#[allow(dead_code)] Timer),
}

impl Handle {
    /// What the handle allows (`pios_abi::rights`).
    pub fn rights(&self) -> usize {
        match self {
            Handle::Process(_) => rights::WAIT | rights::TRANSFER,
            Handle::Endpoint(endpoint) => endpoint.rights(),
            Handle::Reply(_) | Handle::Interrupt(_) | Handle::Timer(_) => rights::TRANSFER,
            Handle::Memory(_) => MEMORY_RIGHTS,
        }
    }

    pub fn has(&self, right: usize) -> bool {
        self.rights() & right == right
    }
}

pub struct HandleTable {
    /// The next number to hand out. Numbers are never reused, so a stale
    /// handle can't silently name a newer object (and closing one twice is
    /// harmless).
    next: usize,
    handles: BTreeMap<usize, Handle>,
}

impl HandleTable {
    pub const fn new() -> HandleTable {
        HandleTable {
            next: 1,
            handles: BTreeMap::new(),
        }
    }

    /// Whether `n` more handles would fit.
    pub fn has_room(&self, n: usize) -> bool {
        self.handles.len() + n <= MAX_HANDLES
    }

    /// Add `handle`, returning its number, or give it back if the table is
    /// full.
    pub fn insert(&mut self, handle: Handle) -> Result<usize, Handle> {
        if !self.has_room(1) {
            return Err(handle);
        }
        let number = self.next;
        self.next += 1;
        self.handles.insert(number, handle);
        Ok(number)
    }

    pub fn get(&self, number: usize) -> Option<&Handle> {
        self.handles.get(&number)
    }

    /// Take handle `number` out of the table.
    pub fn remove(&mut self, number: usize) -> Option<Handle> {
        self.handles.remove(&number)
    }

    /// Take handle `number` out of the table if `check` accepts it.
    pub fn remove_if<E>(
        &mut self,
        number: usize,
        check: impl FnOnce(&Handle) -> Result<(), E>,
    ) -> Option<Result<Handle, E>> {
        let handle = self.handles.get(&number)?;
        Some(check(handle).map(|()| self.handles.remove(&number).expect("just found")))
    }
}
