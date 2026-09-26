//! Handles and IPC.
//!
//! A [`Handle`] owns one entry in this process's handle table and closes it
//! when dropped, so handles can't leak. Sending a handle in a [`Message`]
//! moves it (a handle in a message that fails to send is closed), and a
//! [`Reply`] can answer its call only once, because answering consumes it.

use pios_abi::{Error, MESSAGE_WORDS, call};

use crate::{syscall, syscall4};

/// A handle to a kernel object. Closed when dropped.
#[derive(Debug, PartialEq, Eq)]
pub struct Handle(usize);

/// Close a raw handle number, ignoring errors. (The kernel never reuses
/// numbers, so closing one that's already gone is harmless.)
pub(crate) fn close_raw(raw: usize) {
    if raw != 0 {
        let _ = syscall(call::CLOSE, raw, 0, 0);
    }
}

impl Handle {
    /// Take ownership of handle number `raw`.
    pub fn from_raw(raw: usize) -> Handle {
        Handle(raw)
    }

    /// The handle's number.
    pub fn raw(&self) -> usize {
        self.0
    }

    /// Give up ownership, returning the number.
    pub fn into_raw(self) -> usize {
        let raw = self.0;
        core::mem::forget(self);
        raw
    }

    /// A copy of this endpoint handle with `rights` (no more than this one
    /// has), stamped with `badge` if it isn't 0.
    pub fn duplicate(&self, rights: usize, badge: u64) -> Result<Handle, Error> {
        syscall(call::DUPLICATE, self.0, rights, badge as usize).map(Handle)
    }

    /// Send `message` through this endpoint, waiting until it is received.
    pub fn send(&self, message: Message) -> Result<(), Error> {
        let (raw, handle) = message.into_raw();
        let result = syscall(call::SEND, self.0, &raw as *const _ as usize, 0);
        if result.is_err() {
            close_raw(handle);
        }
        result.map(|_| ())
    }

    /// Send `message` through this endpoint and wait for the reply.
    pub fn call(&self, message: Message) -> Result<Message, Error> {
        let (mut raw, handle) = message.into_raw();
        let result = syscall(call::CALL, self.0, &mut raw as *mut _ as usize, 0);
        if result.is_err() {
            close_raw(handle);
        }
        result.map(|_| Message::from_raw(&raw))
    }

    /// Wait for a message on this endpoint.
    pub fn receive(&self) -> Result<Received, Error> {
        let mut raw = pios_abi::Message::default();
        syscall(call::RECEIVE, self.0, &mut raw as *mut _ as usize, 0)?;
        Ok(Received {
            message: Message::from_raw(&raw),
            badge: raw.badge,
            reply: (raw.reply != 0).then(|| Reply(Handle(raw.reply as usize))),
        })
    }
}

impl Handle {
    /// Map the memory this handle refers to at `address` (page-aligned).
    pub fn map(&self, address: usize) -> Result<(), Error> {
        syscall(call::MAP, self.0, address, 0).map(|_| ())
    }

    /// Deliver this interrupt to `endpoint` as notifications with `badge`,
    /// and enable it.
    pub fn bind_interrupt(&self, endpoint: &Handle, badge: u64) -> Result<(), Error> {
        syscall(call::INTERRUPT_BIND, self.0, endpoint.0, badge as usize).map(|_| ())
    }

    /// Map `pages` fresh pages of memory for a device's DMA at `address`
    /// (through this DMA handle), uncached; returns their bus address.
    pub fn dma_alloc(&self, address: usize, pages: usize) -> Result<u64, Error> {
        syscall(call::DMA_ALLOC, self.0, address, pages).map(|bus| bus as u64)
    }

    /// This interrupt has been dealt with: unmask it.
    pub fn acknowledge_interrupt(&self) -> Result<(), Error> {
        syscall(call::INTERRUPT_ACK, self.0, 0, 0).map(|_| ())
    }
}

/// Notify `endpoint` with `badge` every `period_ms` milliseconds, until the
/// returned handle is dropped.
pub fn timer(endpoint: &Handle, badge: u64, period_ms: u64) -> Result<Handle, Error> {
    syscall(call::TIMER, endpoint.0, badge as usize, period_ms as usize).map(Handle)
}

impl Drop for Handle {
    fn drop(&mut self) {
        close_raw(self.0);
    }
}

/// Make a new endpoint, returning a handle to it with every right.
pub fn endpoint() -> Result<Handle, Error> {
    syscall4(call::ENDPOINT, [0; 4]).map(Handle)
}

/// A message: a label saying what it means, data words, and perhaps a
/// handle to pass on.
#[derive(Debug, Default)]
pub struct Message {
    pub label: u64,
    pub data: [u64; MESSAGE_WORDS],
    pub handle: Option<Handle>,
}

impl Message {
    /// A message with `label` and the first data words from `data`.
    pub fn new(label: u64, data: &[u64]) -> Message {
        let mut message = Message {
            label,
            ..Message::default()
        };
        message.data[..data.len()].copy_from_slice(data);
        message
    }

    /// This message, carrying `handle` too.
    pub fn with_handle(mut self, handle: Handle) -> Message {
        self.handle = Some(handle);
        self
    }

    fn into_raw(self) -> (pios_abi::Message, usize) {
        let handle = self.handle.map_or(0, Handle::into_raw);
        let raw = pios_abi::Message {
            label: self.label,
            handle: handle as u64,
            data: self.data,
            ..pios_abi::Message::default()
        };
        (raw, handle)
    }

    fn from_raw(raw: &pios_abi::Message) -> Message {
        Message {
            label: raw.label,
            data: raw.data,
            handle: (raw.handle != 0).then(|| Handle(raw.handle as usize)),
        }
    }
}

/// A received message.
#[derive(Debug)]
pub struct Received {
    pub message: Message,
    /// The badge of the handle the sender used (0 if none).
    pub badge: u64,
    /// For a call, the way to answer it.
    pub reply: Option<Reply>,
}

/// The way to answer one call. Dropping it unanswered tells the caller the
/// call failed (`PeerGone`).
#[derive(Debug)]
pub struct Reply(Handle);

impl Reply {
    /// Send the reply. If that fails, the call fails too (`PeerGone`).
    pub fn reply(self, message: Message) -> Result<(), Error> {
        let (raw, handle) = message.into_raw();
        let reply = self.0.into_raw();
        let result = syscall(call::REPLY, reply, &raw as *const _ as usize, 0);
        if result.is_err() {
            close_raw(handle);
            close_raw(reply);
        }
        result.map(|_| ())
    }

    /// The reply handle, e.g. to pass it to another process to answer.
    pub fn into_handle(self) -> Handle {
        self.0
    }
}
