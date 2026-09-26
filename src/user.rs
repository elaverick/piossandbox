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

/// Check that the running program could make `kind` of access to every
/// page of `[addr, addr + len)`. An empty range touches no memory, so any
/// address will do (Rust uses a dangling placeholder pointer for empty
/// slices).
fn check(addr: usize, len: usize, kind: Probe) -> Result<(), Error> {
    if len == 0 {
        return Ok(());
    }
    let end = addr.checked_add(len).ok_or(Error::BadAddress)?;
    if end > USER_END {
        return Err(Error::BadAddress);
    }
    let mut page = addr & !(PAGE_SIZE - 1);
    while page < end {
        mmu::probe(page, kind).map_err(|_| Error::BadAddress)?;
        page += PAGE_SIZE;
    }
    Ok(())
}

impl UserSlice {
    /// Check `[addr, addr + len)` with the MMU, as a user-mode read.
    pub fn new(addr: usize, len: usize) -> Result<UserSlice, Error> {
        check(addr, len, Probe::UserRead)?;
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
        // nothing can have changed its mappings since: only the process's
        // own thread could, and it is in this system call. (Other threads
        // may run meanwhile, but switching back restores this process's
        // address space.)
        unsafe {
            core::ptr::copy_nonoverlapping(
                (self.addr + offset) as *const u8,
                dst.as_mut_ptr(),
                dst.len(),
            )
        };
    }
}

/// A range of the running program's memory that it may write.
pub struct UserSliceMut {
    addr: usize,
    len: usize,
}

impl UserSliceMut {
    /// Check `[addr, addr + len)` with the MMU, as a user-mode write.
    pub fn new(addr: usize, len: usize) -> Result<UserSliceMut, Error> {
        check(addr, len, Probe::UserWrite)?;
        Ok(UserSliceMut { addr, len })
    }

    /// Copy `src` to `offset`.
    ///
    /// # Panics
    ///
    /// If that runs past the end of the slice.
    pub fn write(&self, offset: usize, src: &[u8]) {
        assert!(
            offset + src.len() <= self.len,
            "write past the end of a user slice"
        );
        // SAFETY: `new` checked the program can write the whole range, and
        // its mappings can't have changed since (see `UserSlice::read`).
        // The kernel never holds references into user memory, so this
        // aliases nothing.
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), (self.addr + offset) as *mut u8, src.len())
        };
    }
}
