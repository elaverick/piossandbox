//! Handle tables: each process's numbered references to kernel objects.
//!
//! A system call names an object by its handle, and holding the handle is
//! the permission to use it; the kernel owns the table, so handles can't be
//! forged. For now the only objects are processes (a parent's handle on a
//! child it started); IPC endpoints, memory and interrupts will follow, with
//! rights on each handle (see docs/design.md).

use alloc::collections::BTreeMap;

use crate::thread::JoinHandle;

/// The most handles one process may hold.
pub const MAX_HANDLES: usize = 1024;

/// What a handle refers to.
pub enum Object {
    /// A process the holder started: it can wait for it to end.
    Process(JoinHandle),
}

pub struct HandleTable {
    /// The next number to hand out. Numbers are never reused, so a stale
    /// handle can't silently name a newer object.
    next: usize,
    objects: BTreeMap<usize, Object>,
}

impl HandleTable {
    pub const fn new() -> HandleTable {
        HandleTable {
            next: 1,
            objects: BTreeMap::new(),
        }
    }

    /// Add `object`, returning its handle, or give it back if the table is
    /// full.
    pub fn insert(&mut self, object: Object) -> Result<usize, Object> {
        if self.is_full() {
            return Err(object);
        }
        let handle = self.next;
        self.next += 1;
        self.objects.insert(handle, object);
        Ok(handle)
    }

    /// Whether another handle would be refused.
    pub fn is_full(&self) -> bool {
        self.objects.len() >= MAX_HANDLES
    }

    /// The object `handle` refers to.
    pub fn get(&self, handle: usize) -> Option<&Object> {
        self.objects.get(&handle)
    }

    /// Take `handle` out of the table.
    pub fn remove(&mut self, handle: usize) -> Option<Object> {
        self.objects.remove(&handle)
    }
}
