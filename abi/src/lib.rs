//! The pios system call interface, shared by the kernel and user programs
//! so the two can't disagree.
//!
//! A system call is `svc #0` with the call number in `x8` and up to six
//! arguments in `x0`-`x5`. The result comes back in `x0`: a non-negative
//! value on success, or a negative [`Error`] code. Other registers are
//! preserved.

#![no_std]

/// System call numbers.
pub mod call {
    /// `debug_write(ptr, len) -> bytes written`: write text to the kernel
    /// console. Temporary, until the console server exists.
    pub const DEBUG_WRITE: usize = 0;
    /// `exit(code) -> !`: end the calling process.
    pub const EXIT: usize = 1;
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
            n if n < 0 => Err(Error::InvalidArgument),
            n => Ok(n as usize),
        }
    }
}
