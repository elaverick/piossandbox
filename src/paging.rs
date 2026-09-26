//! AArch64 page tables: 4 KiB granule, 39-bit address ranges, three levels
//! (level 1 entries cover 1 GiB, level 2 2 MiB, level 3 4 KiB).
//!
//! Permissions are typed so that bad combinations can't be written:
//! `Access` only offers read-only, read-write and read-execute (never
//! write and execute together), device memory is never executable, and a
//! table only accepts addresses from its own half of the address space.
//!
//! This file depends only on `core` (and `addr`), so the host unit tests
//! compile it too. Where tables live is up to a `TableMemory`; TLB
//! maintenance is up to the caller.

use crate::addr::{PAGE_SIZE, PhysAddr};

pub const ENTRIES: usize = 512;
/// Size of each half of the address space.
pub const HALF_SIZE: usize = 1 << 39;
/// Where the upper (kernel) half starts.
pub const UPPER_HALF_START: usize = 0usize.wrapping_sub(HALF_SIZE);

const LEVEL_SHIFT: [u32; 3] = [30, 21, 12];

/// Kinds of memory, indexing the MAIR_EL1 value set up in boot.s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryType {
    /// Ordinary RAM, write-back cached.
    Normal,
    /// RAM shared with a device that bypasses the caches (write-combining).
    NonCacheable,
    /// Device registers: no caching, merging, reordering or speculation.
    Device,
}

impl MemoryType {
    const fn mair_index(self) -> u64 {
        match self {
            MemoryType::Device => 0,
            MemoryType::Normal => 1,
            MemoryType::NonCacheable => 2,
        }
    }
}

/// Who may access a page and how. Every page is readable; the constants
/// below are the only ways to make one, and none is both writable and
/// executable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    write: bool,
    execute: bool,
    user: bool,
}

impl Access {
    pub const KERNEL_READ: Access = Access {
        write: false,
        execute: false,
        user: false,
    };
    pub const KERNEL_READ_WRITE: Access = Access {
        write: true,
        execute: false,
        user: false,
    };
    pub const KERNEL_READ_EXECUTE: Access = Access {
        write: false,
        execute: true,
        user: false,
    };
    pub const USER_READ: Access = Access {
        write: false,
        execute: false,
        user: true,
    };
    pub const USER_READ_WRITE: Access = Access {
        write: true,
        execute: false,
        user: true,
    };
    pub const USER_READ_EXECUTE: Access = Access {
        write: false,
        execute: true,
        user: true,
    };

    pub const fn is_writable(self) -> bool {
        self.write
    }

    pub const fn is_executable(self) -> bool {
        self.execute
    }

    /// Accessible from user mode (EL0)? The kernel can access user pages
    /// too, but never executes them.
    pub const fn is_user(self) -> bool {
        self.user
    }
}

/// How a range of memory is mapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attributes {
    memory: MemoryType,
    access: Access,
}

impl Attributes {
    pub const fn normal(access: Access) -> Self {
        Attributes {
            memory: MemoryType::Normal,
            access,
        }
    }

    /// Non-cacheable memory, never executable.
    pub const fn non_cacheable(access: Access) -> Self {
        Attributes {
            memory: MemoryType::NonCacheable,
            access: Access {
                execute: false,
                ..access
            },
        }
    }

    /// Device registers, never executable (instruction fetches from devices
    /// could read registers speculatively).
    pub const fn device(access: Access) -> Self {
        Attributes {
            memory: MemoryType::Device,
            access: Access {
                execute: false,
                ..access
            },
        }
    }

    pub const fn memory(self) -> MemoryType {
        self.memory
    }

    pub const fn access(self) -> Access {
        self.access
    }
}

// Descriptor fields.
const VALID: u64 = 1 << 0;
const TABLE_OR_PAGE: u64 = 1 << 1; // table at levels 1-2, page at level 3
const ATTR_INDEX_SHIFT: u32 = 2;
const AP_USER: u64 = 1 << 6; // AP[1]: EL0 may access
const AP_READ_ONLY: u64 = 1 << 7; // AP[2]
const SHAREABLE_INNER: u64 = 0b11 << 8;
const ACCESS_FLAG: u64 = 1 << 10;
const NOT_GLOBAL: u64 = 1 << 11;
const PXN: u64 = 1 << 53; // never executable at EL1
const UXN: u64 = 1 << 54; // never executable at EL0
const ADDRESS_MASK: u64 = 0x0000_FFFF_FFFF_F000;

impl Attributes {
    /// The attribute bits of a block or page descriptor.
    fn descriptor_bits(self) -> u64 {
        let a = self.access;
        let mut bits = ACCESS_FLAG | (self.memory.mair_index() << ATTR_INDEX_SHIFT);
        if self.memory != MemoryType::Device {
            bits |= SHAREABLE_INNER;
        }
        if !a.write {
            bits |= AP_READ_ONLY;
        }
        if a.user {
            // User pages: per-process (not global), never run by the kernel.
            bits |= AP_USER | NOT_GLOBAL | PXN;
            if !a.execute {
                bits |= UXN;
            }
        } else {
            bits |= UXN;
            if !a.execute {
                bits |= PXN;
            }
        }
        bits
    }

    fn from_descriptor(d: u64) -> Self {
        let memory = match (d >> ATTR_INDEX_SHIFT) & 0b111 {
            0 => MemoryType::Device,
            2 => MemoryType::NonCacheable,
            _ => MemoryType::Normal,
        };
        let user = d & AP_USER != 0;
        let execute = if user { d & UXN == 0 } else { d & PXN == 0 };
        Attributes {
            memory,
            access: Access {
                write: d & AP_READ_ONLY == 0,
                execute,
                user,
            },
        }
    }
}

/// Which half of the address space a table translates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Half {
    /// `0` to `HALF_SIZE`: user processes (TTBR0).
    Lower,
    /// `UPPER_HALF_START` to the top: the kernel (TTBR1).
    Upper,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// Addresses or size aren't page-aligned.
    Misaligned,
    /// The range isn't in this table's half of the address space.
    OutOfRange,
    /// Part of the range is already mapped.
    AlreadyMapped,
    /// Part of the range isn't mapped.
    NotMapped,
    /// Unmapping would cut a block mapping in two.
    SplitsBlock,
    /// No memory for a new table.
    OutOfMemory,
}

/// Where page tables live.
///
/// # Safety
///
/// `table` must return a pointer valid for reads and writes of a whole table
/// for any address `allocate_table` returned and `free_table` hasn't been
/// given, and nothing else may use that memory meanwhile.
pub unsafe trait TableMemory {
    /// A new, zeroed, page-aligned table.
    fn allocate_table(&mut self) -> Option<PhysAddr>;

    /// # Safety
    ///
    /// `table` came from `allocate_table` and is no longer referenced.
    unsafe fn free_table(&mut self, table: PhysAddr);

    fn table(&self, table: PhysAddr) -> *mut [u64; ENTRIES];
}

/// A tree of page tables for one half of the address space. Dropping it
/// frees all its tables (not the memory they map).
pub struct PageTable<M: TableMemory> {
    root: PhysAddr,
    half: Half,
    memory: M,
}

impl<M: TableMemory> PageTable<M> {
    pub fn new(half: Half, mut memory: M) -> Result<Self, MapError> {
        let root = memory.allocate_table().ok_or(MapError::OutOfMemory)?;
        Ok(PageTable { root, half, memory })
    }

    /// The top-level table, for TTBR0 or TTBR1.
    pub fn root(&self) -> PhysAddr {
        self.root
    }

    pub fn half(&self) -> Half {
        self.half
    }

    /// Offset of `va` within this table's half, or `OutOfRange`.
    fn offset(&self, va: usize, size: usize) -> Result<usize, MapError> {
        let offset = match self.half {
            Half::Lower if va < HALF_SIZE => va,
            Half::Upper if va >= UPPER_HALF_START => va - UPPER_HALF_START,
            _ => return Err(MapError::OutOfRange),
        };
        match offset.checked_add(size) {
            Some(end) if end <= HALF_SIZE => Ok(offset),
            _ => Err(MapError::OutOfRange),
        }
    }

    fn read(&self, table: PhysAddr, index: usize) -> u64 {
        // SAFETY: `table` is one of ours (TableMemory contract) and
        // `index` < ENTRIES. The hardware walks tables, so access them with
        // volatile operations.
        unsafe { (&raw const (*self.memory.table(table))[index]).read_volatile() }
    }

    fn write(&mut self, table: PhysAddr, index: usize, value: u64) {
        // SAFETY: as in `read`.
        unsafe { (&raw mut (*self.memory.table(table))[index]).write_volatile(value) }
    }

    /// Map `[va, va + size)` to `[pa, pa + size)`. With `blocks`, 1 GiB and
    /// 2 MiB blocks are used where the addresses allow; otherwise only 4 KiB
    /// pages (which can be unmapped one at a time). Nothing in the range may
    /// already be mapped; on any error, nothing is left mapped.
    pub fn map(
        &mut self,
        va: usize,
        pa: PhysAddr,
        size: usize,
        attrs: Attributes,
        blocks: bool,
    ) -> Result<(), MapError> {
        if !va.is_multiple_of(PAGE_SIZE) || !pa.is_page_aligned() || !size.is_multiple_of(PAGE_SIZE)
        {
            return Err(MapError::Misaligned);
        }
        let offset = self.offset(va, size)?;
        let mut done = 0;
        while done < size {
            match self.map_one(offset + done, pa + done, size - done, attrs, blocks) {
                Ok(step) => done += step,
                Err(e) => {
                    // Undo what this call mapped. (It all succeeded, so this
                    // can't fail, and it leaves only empty tables behind.)
                    let _ = self.unmap_offset(offset, done);
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Map one page or block at `offset`, returning its size.
    fn map_one(
        &mut self,
        offset: usize,
        pa: PhysAddr,
        remaining: usize,
        attrs: Attributes,
        blocks: bool,
    ) -> Result<usize, MapError> {
        let mut table = self.root;
        for (level, &shift) in LEVEL_SHIFT.iter().enumerate() {
            let size = 1usize << shift;
            let index = (offset >> shift) % ENTRIES;
            let entry = self.read(table, index);
            let last = level == LEVEL_SHIFT.len() - 1;
            let fits = offset.is_multiple_of(size)
                && pa.as_usize().is_multiple_of(size)
                && remaining >= size;
            if last || (blocks && fits && entry == 0) {
                if entry != 0 {
                    return Err(MapError::AlreadyMapped);
                }
                let kind = if last { TABLE_OR_PAGE } else { 0 };
                self.write(
                    table,
                    index,
                    pa.as_usize() as u64 | attrs.descriptor_bits() | kind | VALID,
                );
                return Ok(size);
            }
            table = match entry {
                0 => {
                    let new = self.memory.allocate_table().ok_or(MapError::OutOfMemory)?;
                    self.write(table, index, new.as_usize() as u64 | TABLE_OR_PAGE | VALID);
                    new
                }
                e if e & (TABLE_OR_PAGE | VALID) == TABLE_OR_PAGE | VALID => {
                    PhysAddr::new((e & ADDRESS_MASK) as usize)
                }
                _ => return Err(MapError::AlreadyMapped), // a block covers it
            };
        }
        unreachable!("level 3 always returns")
    }

    /// Remove the mappings for `[va, va + size)`, all of which must exist;
    /// if any don't, nothing is removed. Blocks can only be removed whole.
    /// The caller must invalidate the TLB.
    pub fn unmap(&mut self, va: usize, size: usize) -> Result<(), MapError> {
        if !va.is_multiple_of(PAGE_SIZE) || !size.is_multiple_of(PAGE_SIZE) {
            return Err(MapError::Misaligned);
        }
        let offset = self.offset(va, size)?;
        self.unmap_offset(offset, size)
    }

    fn unmap_offset(&mut self, offset: usize, size: usize) -> Result<(), MapError> {
        // Check the whole range first, so that on error nothing has changed.
        self.walk_mappings(offset, size, |_, _, _| {})?;
        self.walk_mappings(offset, size, |table, index, this| {
            this.write(table, index, 0)
        })
    }

    /// Call `f` with the table and index of each page or block mapping
    /// that makes up `[offset, offset + size)`, which must be exactly
    /// covered by existing mappings.
    fn walk_mappings(
        &mut self,
        offset: usize,
        size: usize,
        mut f: impl FnMut(PhysAddr, usize, &mut Self),
    ) -> Result<(), MapError> {
        let mut done = 0;
        'next: while done < size {
            let at = offset + done;
            let mut table = self.root;
            for (level, &shift) in LEVEL_SHIFT.iter().enumerate() {
                let block = 1usize << shift;
                let index = (at >> shift) % ENTRIES;
                let entry = self.read(table, index);
                if entry & VALID == 0 {
                    return Err(MapError::NotMapped);
                }
                let last = level == LEVEL_SHIFT.len() - 1;
                if last || entry & TABLE_OR_PAGE == 0 {
                    if !at.is_multiple_of(block) || size - done < block {
                        return Err(MapError::SplitsBlock);
                    }
                    f(table, index, self);
                    done += block;
                    continue 'next;
                }
                table = PhysAddr::new((entry & ADDRESS_MASK) as usize);
            }
        }
        Ok(())
    }

    /// Where `va` is mapped to, and how.
    pub fn translate(&self, va: usize) -> Option<(PhysAddr, Attributes)> {
        let offset = self.offset(va, 0).ok()?;
        let mut table = self.root;
        for (level, &shift) in LEVEL_SHIFT.iter().enumerate() {
            let entry = self.read(table, (offset >> shift) % ENTRIES);
            if entry & VALID == 0 {
                return None;
            }
            let last = level == LEVEL_SHIFT.len() - 1;
            if last || entry & TABLE_OR_PAGE == 0 {
                let base = (entry & ADDRESS_MASK) as usize;
                let within = offset & ((1usize << shift) - 1);
                return Some((
                    PhysAddr::new(base + within),
                    Attributes::from_descriptor(entry),
                ));
            }
            table = PhysAddr::new((entry & ADDRESS_MASK) as usize);
        }
        None
    }

    /// Free `table` and every table below it.
    fn free_tree(&mut self, table: PhysAddr, level: usize) {
        if level < LEVEL_SHIFT.len() - 1 {
            for index in 0..ENTRIES {
                let entry = self.read(table, index);
                if entry & (TABLE_OR_PAGE | VALID) == TABLE_OR_PAGE | VALID {
                    self.free_tree(PhysAddr::new((entry & ADDRESS_MASK) as usize), level + 1);
                }
            }
        }
        // SAFETY: the table came from `allocate_table`, and its parent entry
        // is going away with the whole tree.
        unsafe { self.memory.free_table(table) };
    }
}

impl<M: TableMemory> Drop for PageTable<M> {
    fn drop(&mut self) {
        self.free_tree(self.root, 0);
    }
}
