//! User address spaces: the lower half of the address space (TTBR0), one
//! per process.

use alloc::collections::BTreeMap;

use crate::addr::{PAGE_SIZE, PhysAddr, VirtAddr};
use crate::memory::OwnedFrame;
use crate::mmu::{self, KernelTableMemory};
use crate::paging::{Access, Attributes, HALF_SIZE, Half, MapError, PageTable};

/// Lowest address a user mapping may use: page 0 stays unmapped so null
/// pointers fault.
pub const USER_START: usize = PAGE_SIZE;
/// One past the highest user address.
pub const USER_END: usize = HALF_SIZE;

/// A user address space. It owns its page tables and the memory mapped
/// into it; dropping it frees both, switching away from it first if it is
/// active.
pub struct AddressSpace {
    table: PageTable<KernelTableMemory>,
    /// The frame behind each mapped page, by virtual address.
    pages: BTreeMap<usize, OwnedFrame>,
}

impl AddressSpace {
    pub fn new() -> Result<Self, MapError> {
        Ok(AddressSpace {
            table: PageTable::new(Half::Lower, KernelTableMemory)?,
            pages: BTreeMap::new(),
        })
    }

    fn check(va: usize, pages: usize) -> Result<(), MapError> {
        if !va.is_multiple_of(PAGE_SIZE) {
            return Err(MapError::Misaligned);
        }
        match pages
            .checked_mul(PAGE_SIZE)
            .and_then(|size| va.checked_add(size))
        {
            Some(end) if va >= USER_START && end <= USER_END => Ok(()),
            _ => Err(MapError::OutOfRange),
        }
    }

    /// Map `pages` new, zeroed pages at `va`, with user `access`. On error
    /// nothing is mapped.
    ///
    /// # Panics
    ///
    /// If `access` isn't a user access: the kernel's own mappings live in
    /// the kernel half.
    pub fn allocate(&mut self, va: usize, pages: usize, access: Access) -> Result<(), MapError> {
        assert!(
            access.is_user(),
            "user address spaces only hold user mappings"
        );
        Self::check(va, pages)?;
        for i in 0..pages {
            let page = va + i * PAGE_SIZE;
            let result = OwnedFrame::allocate()
                .ok_or(MapError::OutOfMemory)
                .and_then(|frame| {
                    self.table.map(
                        page,
                        frame.addr(),
                        PAGE_SIZE,
                        Attributes::normal(access),
                        false,
                    )?;
                    Ok(frame)
                });
            match result {
                Ok(frame) => {
                    self.pages.insert(page, frame);
                }
                Err(e) => {
                    // Undo the pages this call mapped.
                    let _ = self.free(va, i);
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Unmap `pages` pages at `va` and free their memory. All must be
    /// mapped; if any aren't, nothing changes.
    pub fn free(&mut self, va: usize, pages: usize) -> Result<(), MapError> {
        Self::check(va, pages)?;
        self.table.unmap(va, pages * PAGE_SIZE)?;
        // No stale translation may reach the frames once they are reused.
        if self.is_active() {
            mmu::flush_user_tlb();
        }
        for i in 0..pages {
            self.pages.remove(&(va + i * PAGE_SIZE));
        }
        Ok(())
    }

    /// Map `data`, which must be whole pages, read-only for user mode at
    /// `va`. Being `'static` and immutable, it is never freed or written, so
    /// it can be shared rather than copied; this address space doesn't own
    /// it and won't free it.
    pub fn map_static(&mut self, va: usize, data: &'static [u8]) -> Result<(), MapError> {
        let start = VirtAddr::from_ptr(data.as_ptr());
        if !start.as_usize().is_multiple_of(PAGE_SIZE) || !data.len().is_multiple_of(PAGE_SIZE) {
            return Err(MapError::Misaligned);
        }
        Self::check(va, data.len() / PAGE_SIZE)?;
        let pa = start.to_phys().ok_or(MapError::OutOfRange)?;
        self.table.map(
            va,
            pa,
            data.len(),
            Attributes::normal(Access::USER_READ),
            false,
        )
    }

    /// Copy `data` into this address space at `va` (through the kernel's
    /// view of the pages, so it works whatever the pages' user
    /// permissions). The pages must be mapped.
    pub fn write(&mut self, va: usize, data: &[u8]) -> Result<(), MapError> {
        let mut done = 0;
        while done < data.len() {
            let at = va + done;
            let page = at & !(PAGE_SIZE - 1);
            let frame = self.pages.get(&page).ok_or(MapError::NotMapped)?;
            let offset = at - page;
            let chunk = (PAGE_SIZE - offset).min(data.len() - done);
            let dst = (frame.addr().to_virt() + offset).as_ptr::<u8>();
            // SAFETY: the frame belongs to this address space and `chunk`
            // stays within it; the kernel reaches it through the linear map.
            unsafe { core::ptr::copy_nonoverlapping(data[done..].as_ptr(), dst, chunk) };
            crate::cache::clean_to_unification(dst as usize, chunk);
            done += chunk;
        }
        Ok(())
    }

    /// The top-level page table, for TTBR0.
    pub fn root(&self) -> PhysAddr {
        self.table.root()
    }

    /// Make this the current lower half.
    pub fn activate(&self) {
        mmu::set_user_tables(Some(self.table.root()));
    }

    pub fn is_active(&self) -> bool {
        mmu::user_tables() == self.table.root()
    }

    /// The physical address behind `va`, if mapped.
    pub fn translate(&self, va: usize) -> Option<PhysAddr> {
        self.table.translate(va).map(|(pa, _)| pa)
    }

    /// Number of pages mapped.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        // Stop using the tables before they (and then the pages) are freed.
        if self.is_active() {
            mmu::set_user_tables(None);
        }
    }
}

/// Check address spaces and the kernel map with the MMU itself: isolation
/// between spaces, permissions, W^X, and that everything is freed.
pub fn self_test() -> Result<(), &'static str> {
    use crate::memory::frame_stats;
    use crate::mmu::{Probe, is_permission_fault, is_translation_fault, kernel_translate, probe};

    // Before any address space is used: the lower half must be empty (the
    // boot identity map gone).
    let idle = crate::mmu::user_tables() == crate::mmu::user_tables_when_idle();
    if !idle || probe(0x8_0000, Probe::KernelRead).is_ok() {
        return Err("the boot identity map is still there");
    }

    let free_before = frame_stats().free;
    let data = 0x40_0000;
    let read_only = data + PAGE_SIZE;
    let unmapped = 0x60_0000;
    {
        let err = |_| "could not build an address space";
        let mut a = AddressSpace::new().map_err(err)?;
        let mut b = AddressSpace::new().map_err(err)?;
        a.allocate(data, 1, Access::USER_READ_WRITE).map_err(err)?;
        a.allocate(read_only, 1, Access::USER_READ).map_err(err)?;
        b.allocate(data, 1, Access::USER_READ_WRITE).map_err(err)?;
        if a.translate(data) == b.translate(data) {
            return Err("two address spaces share a page");
        }

        // The same address holds different memory in each space.
        let ptr = data as *mut u64;
        a.activate();
        // SAFETY: `data` is mapped read-write in `a`, which is active.
        unsafe { ptr.write_volatile(0xAAAA_0001) };
        b.activate();
        // SAFETY: likewise in `b`.
        unsafe { ptr.write_volatile(0xBBBB_0002) };
        a.activate();
        // SAFETY: `a` is active again.
        if unsafe { ptr.read_volatile() } != 0xAAAA_0001 {
            return Err("address spaces are not isolated");
        }

        // Permissions, as seen from user mode (EL0) and the kernel (EL1).
        let checks = [
            (
                probe(data, Probe::UserWrite).is_ok(),
                "user can't write its own page",
            ),
            (
                probe(read_only, Probe::UserWrite).is_err_and(is_permission_fault),
                "user can write a read-only page",
            ),
            (
                probe(unmapped, Probe::UserRead).is_err_and(is_translation_fault),
                "an unmapped page is readable",
            ),
            (probe(0, Probe::UserRead).is_err(), "page 0 is readable"),
            (
                probe(
                    crate::mmu::kernel_image().0.to_virt().as_usize(),
                    Probe::UserRead,
                )
                .is_err(),
                "user can read the kernel",
            ),
        ];
        if let Some((_, message)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(message);
        }

        a.free(read_only, 1).map_err(|_| "could not free a page")?;
        if a.page_count() != 1
            || a.translate(read_only).is_some()
            || probe(read_only, Probe::UserRead).is_ok()
        {
            return Err("a freed page is still mapped");
        }
        // `a` is dropped while active: it must switch itself off first.
    }
    if crate::mmu::user_tables() != crate::mmu::user_tables_when_idle() {
        return Err("a dropped address space was left active");
    }
    if frame_stats().free != free_before {
        return Err("address spaces leaked memory");
    }

    // The kernel's own map: code can't be written, data can't be run.
    let code = VirtAddr::from_ptr(self_test as *const u8);
    static DATA: u8 = 0;
    let checks = [
        (
            probe(code.as_usize(), Probe::KernelRead).is_ok(),
            "kernel code unreadable",
        ),
        (
            probe(code.as_usize(), Probe::KernelWrite).is_err_and(is_permission_fault),
            "kernel code is writable",
        ),
        (
            kernel_translate(code).is_some_and(|(_, a)| a.access().is_executable()),
            "kernel code isn't executable",
        ),
        (
            kernel_translate(VirtAddr::from_ptr(&DATA))
                .is_some_and(|(_, a)| !a.access().is_executable()),
            "kernel data is executable",
        ),
        (
            kernel_translate(VirtAddr::from_ptr(&free_before))
                .is_some_and(|(_, a)| !a.access().is_executable()),
            "the kernel stack is executable",
        ),
    ];
    match checks.iter().find(|(ok, _)| !ok) {
        Some((_, message)) => Err(message),
        None => Ok(()),
    }
}
