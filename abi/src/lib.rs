//! The pios system call interface, shared by the kernel and user programs
//! so the two can't disagree.
//!
//! A system call is `svc #0` with the call number in `x8` and up to six
//! arguments in `x0`-`x5`. The result comes back in `x0`: a non-negative
//! value on success, or a negative [`Error`] code. Other registers are
//! preserved.
//!
//! A program's entry point gets two arguments from whoever started it: a
//! number in `x0`, and in `x1` a handle it was given (0 if none).
//!
//! Handles name kernel objects; see [`rights`] for what each allows, and
//! [`Message`] for inter-process communication.
//!
//! Programs may read the time directly from the ARM generic timer's
//! virtual counter (`CNTVCT_EL0`, ticking at `CNTFRQ_EL0` Hz).

#![no_std]

/// System call numbers.
pub mod call {
    /// `debug_write(ptr, len) -> bytes written`: write text to the kernel
    /// console. Temporary, until the console server exists.
    pub const DEBUG_WRITE: usize = 0;
    /// `exit(code) -> !`: end the calling process.
    pub const EXIT: usize = 1;
    /// `yield() -> 0`: let other threads run for the rest of this time
    /// slice.
    pub const YIELD: usize = 2;
    /// `spawn(ptr, len, arg, handle) -> handle`: start a new process
    /// running the ELF executable in `[ptr, ptr + len)` of the caller's
    /// memory, with `arg` as its entry point's first argument. If `handle`
    /// isn't 0, it is moved to the new process (it needs the `TRANSFER`
    /// right), which gets its number there as its second argument. Returns
    /// a handle to the new process.
    pub const SPAWN: usize = 3;
    /// `wait(handle) -> status`: wait for the process `handle` refers to to
    /// end, then close the handle. Returns an [`ExitStatus`](super::ExitStatus)
    /// in raw form.
    pub const WAIT: usize = 4;
    /// `close(handle) -> 0`: give up a handle. (Closing a process handle
    /// doesn't stop the process.)
    pub const CLOSE: usize = 5;
    /// `endpoint() -> handle`: make a new IPC endpoint, returning a handle
    /// to it with every right.
    pub const ENDPOINT: usize = 6;
    /// `duplicate(handle, rights, badge) -> handle`: a copy of an endpoint
    /// handle with the same or fewer rights (needs `DUPLICATE`). A nonzero
    /// `badge` stamps the copy, which is only allowed if the original has
    /// no badge; 0 keeps the original's.
    pub const DUPLICATE: usize = 7;
    /// `send(endpoint, message) -> 0`: send the [`Message`](super::Message)
    /// at `message` and wait until a receiver has taken it. Needs `SEND`.
    pub const SEND: usize = 8;
    /// `call(endpoint, message) -> 0`: send the message and wait for the
    /// reply, which replaces it. Needs `SEND`.
    pub const CALL: usize = 9;
    /// `receive(endpoint, message) -> 0`: wait for a message and store it
    /// at `message`, with the sender's badge, and for a call a reply handle.
    /// Needs `RECEIVE`.
    pub const RECEIVE: usize = 10;
    /// `reply(reply, message) -> 0`: answer a call through its reply
    /// handle, which this uses up. Doesn't wait.
    pub const REPLY: usize = 11;
}

/// What a handle allows. An endpoint handle can have any of the first four;
/// a process handle has `WAIT` and `TRANSFER`, a reply handle `TRANSFER`.
pub mod rights {
    /// Send and call through the endpoint.
    pub const SEND: usize = 1 << 0;
    /// Receive from the endpoint.
    pub const RECEIVE: usize = 1 << 1;
    /// Make copies of the handle (with the same or fewer rights).
    pub const DUPLICATE: usize = 1 << 2;
    /// Pass the handle to another process, in a message or to `spawn`.
    pub const TRANSFER: usize = 1 << 3;
    /// Wait for the process to end.
    pub const WAIT: usize = 1 << 4;
    /// Every right an endpoint handle can have.
    pub const ENDPOINT_ALL: usize = SEND | RECEIVE | DUPLICATE | TRANSFER;
}

/// The data words in a message.
pub const MESSAGE_WORDS: usize = 12;
/// The size of a [`Message`] in memory.
pub const MESSAGE_SIZE: usize = 128;

/// An IPC message, as system calls read and write it in the caller's
/// memory. The kernel copies it from sender to receiver.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Message {
    /// What the message means: the operation, or for replies the result.
    /// The kernel doesn't look at it.
    pub label: u64,
    /// A handle to move to the receiver (it needs the `TRANSFER` right), or
    /// 0. On receipt, its number in the receiver's table, or 0. A handle in
    /// a message that is never delivered is closed.
    pub handle: u64,
    /// On receipt: the badge of the handle the sender used (0 if none).
    /// Ignored when sending.
    pub badge: u64,
    /// On receipt of a call: the reply handle, to answer it with. Otherwise
    /// 0. Ignored when sending.
    pub reply: u64,
    pub data: [u64; MESSAGE_WORDS],
}

const _: () = assert!(core::mem::size_of::<Message>() == MESSAGE_SIZE);

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

    /// The message as bytes, as it is laid out in memory.
    pub fn to_bytes(&self) -> [u8; MESSAGE_SIZE] {
        let mut bytes = [0; MESSAGE_SIZE];
        let words = [self.label, self.handle, self.badge, self.reply]
            .into_iter()
            .chain(self.data);
        for (chunk, word) in bytes.chunks_exact_mut(8).zip(words) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    pub fn from_bytes(bytes: &[u8; MESSAGE_SIZE]) -> Message {
        let mut words = bytes
            .chunks_exact(8)
            .map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap()));
        let mut next = || words.next().unwrap();
        let (label, handle, badge, reply) = (next(), next(), next(), next());
        Message {
            label,
            handle,
            badge,
            reply,
            data: core::array::from_fn(|_| next()),
        }
    }
}

/// The largest executable `spawn` accepts.
pub const SPAWN_MAX: usize = 4 << 20;

/// How a process ended, as `wait` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    /// It called `exit` with this code (or returned it from `main`).
    Code(i32),
    /// It was stopped by a fault; `esr` is the exception syndrome (the low
    /// 32 bits of ESR_EL1), which says what kind.
    Fault { esr: u32 },
}

impl ExitStatus {
    const FAULT: usize = 1 << 32;

    /// The system call result for this status (never negative).
    pub const fn to_raw(self) -> usize {
        match self {
            ExitStatus::Code(code) => code as u32 as usize,
            ExitStatus::Fault { esr } => Self::FAULT | esr as usize,
        }
    }

    /// Decode a `wait` result.
    pub const fn from_raw(raw: usize) -> Option<ExitStatus> {
        match raw >> 32 {
            0 => Some(ExitStatus::Code(raw as u32 as i32)),
            1 => Some(ExitStatus::Fault { esr: raw as u32 }),
            _ => None,
        }
    }
}

/// The largest single `debug_write`.
pub const DEBUG_WRITE_MAX: usize = 64 * 1024;

/// System call errors, returned as negative values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(isize)]
pub enum Error {
    /// No system call has that number.
    NoSuchCall = -1,
    /// An argument is out of range.
    InvalidArgument = -2,
    /// A pointer argument doesn't point at memory the caller may use.
    BadAddress = -3,
    /// No handle with that number, or not the right kind of object.
    BadHandle = -4,
    /// The kernel ran out of memory for the request, or the caller's
    /// handle table is full.
    OutOfMemory = -5,
    /// The other side is gone: every handle that could receive (for a
    /// send or call), or send (for a receive), has been closed; or the
    /// server dropped a call's reply handle without replying.
    PeerGone = -6,
    /// The handle doesn't have the right needed.
    AccessDenied = -7,
}

impl Error {
    /// The register value for this error.
    pub const fn to_raw(self) -> usize {
        self as isize as usize
    }

    /// Decode a system call's result register.
    pub const fn from_result(raw: usize) -> Result<usize, Error> {
        match raw as isize {
            -1 => Err(Error::NoSuchCall),
            -2 => Err(Error::InvalidArgument),
            -3 => Err(Error::BadAddress),
            -4 => Err(Error::BadHandle),
            -5 => Err(Error::OutOfMemory),
            -6 => Err(Error::PeerGone),
            -7 => Err(Error::AccessDenied),
            n if n < 0 => Err(Error::InvalidArgument),
            n => Ok(n as usize),
        }
    }
}
