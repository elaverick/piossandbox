//! Access to user memory from the kernel.
//!
//! System call arguments that point into user memory can't be trusted: they
//! might point at the kernel, at nothing, or run off the end of the address
//! space. A `UserSlice` can only be made after the MMU has confirmed that
//! the running program itself could read every page of the range.

use pios_abi::Error;

use crate::addr::PAGE_SIZE;
use crate::addrspace::USER_END;
use crate::mmu::{self, Probe};

/// A range of the running program's memory that it may read.
pub struct UserSlice {
    addr: usize,
    len: usize,
}

impl UserSlice {
    /// Check `[addr, addr + len)` with the MMU, as a user-mode read. An
    /// empty range touches no memory, so any address will do (Rust uses a
    /// dangling placeholder pointer for empty slices).
    pub fn new(addr: usize, len: usize) -> Result<UserSlice, Error> {
        if len == 0 {
            return Ok(UserSlice { addr, len });
        }
        let end = addr.checked_add(len).ok_or(Error::BadAddress)?;
        if end > USER_END {
            return Err(Error::BadAddress);
        }
        let mut page = addr & !(PAGE_SIZE - 1);
        while page < end {
            mmu::probe(page, Probe::UserRead).map_err(|_| Error::BadAddress)?;
            page += PAGE_SIZE;
        }
        Ok(UserSlice { addr, len })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Copy `dst.len()` bytes from `offset` into `dst`.
    ///
    /// # Panics
    ///
    /// If that runs past the end of the slice.
    pub fn read(&self, offset: usize, dst: &mut [u8]) {
        assert!(
            offset + dst.len() <= self.len,
            "read past the end of a user slice"
        );
        // SAFETY: `new` checked the program can read the whole range, and
        // nothing can change its mappings while the kernel runs (one core,
        // and the program is stopped in this system call).
        unsafe {
            core::ptr::copy_nonoverlapping(
                (self.addr + offset) as *const u8,
                dst.as_mut_ptr(),
                dst.len(),
            )
        };
    }
}
