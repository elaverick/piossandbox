//! The pios system call interface, shared by the kernel and user programs
//! so the two can't disagree.
//!
//! A system call is `svc #0` with the call number in `x8` and up to six
//! arguments in `x0`-`x5`. The result comes back in `x0`: a non-negative
//! value on success, or a negative [`Error`] code. Other registers are
//! preserved.
//!
//! A program's entry point gets one argument, in `x0`, from whoever
//! started it.
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
    /// `spawn(ptr, len, arg) -> handle`: start a new process running the
    /// ELF executable in `[ptr, ptr + len)` of the caller's memory, with
    /// `arg` as its entry point's argument. Returns a handle to it.
    pub const SPAWN: usize = 3;
    /// `wait(handle) -> status`: wait for the process `handle` refers to to
    /// end, then close the handle. Returns an [`ExitStatus`](super::ExitStatus)
    /// in raw form.
    pub const WAIT: usize = 4;
    /// `close(handle) -> 0`: give up a handle. (Closing a process handle
    /// doesn't stop the process.)
    pub const CLOSE: usize = 5;
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
    /// The kernel ran out of memory for the request.
    OutOfMemory = -5,
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
            n if n < 0 => Err(Error::InvalidArgument),
            n => Ok(n as usize),
        }
    }
}
