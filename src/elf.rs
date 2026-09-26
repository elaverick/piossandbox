//! Parsing ELF executables for loading as user programs.
//!
//! Only what pios needs: 64-bit little-endian AArch64 executables with
//! loadable segments. Written in safe Rust over a byte slice; `Elf::new`
//! checks everything the loader relies on, so a malformed or hostile file is
//! refused rather than half-loaded:
//!
//! - every segment's file data lies within the file;
//! - every segment lies within `[lowest, highest)` (user space), without
//!   overflow, and page-aligned segments don't share pages;
//! - no segment is both writable and executable (W^X);
//! - the entry point is inside an executable segment.
//!
//! This file depends only on `core`, so the host unit tests compile it too.

const PAGE_SIZE: u64 = 4096;

const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const CLASS_64: u8 = 2;
const LITTLE_ENDIAN: u8 = 1;
const TYPE_EXECUTABLE: u16 = 2;
const MACHINE_AARCH64: u16 = 183;
const PT_LOAD: u32 = 1;
const HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;
/// More segments than any sensible program has.
const MAX_SEGMENTS: usize = 16;

pub const FLAG_EXECUTE: u32 = 1;
pub const FLAG_WRITE: u32 = 2;
pub const FLAG_READ: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElfError {
    /// Not an ELF file, or not 64-bit little-endian AArch64.
    NotAArch64Elf,
    /// Not an executable (e.g. a shared library or object file).
    NotExecutable,
    /// A header or segment points outside the file.
    Truncated,
    /// A segment is outside the allowed address range, or overflows.
    BadAddress,
    /// Two segments share a page.
    Overlap,
    /// A segment is both writable and executable.
    WritableAndExecutable,
    /// The entry point isn't in an executable segment.
    BadEntry,
    /// No loadable segments, or too many.
    BadSegmentCount,
}

/// A loadable segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment<'a> {
    /// Where it goes in memory.
    pub vaddr: u64,
    /// Its size in memory; past `data`, it is zero-filled.
    pub mem_size: u64,
    /// Its contents from the file.
    pub data: &'a [u8],
    pub flags: u32,
}

impl Segment<'_> {
    pub fn is_writable(&self) -> bool {
        self.flags & FLAG_WRITE != 0
    }

    pub fn is_executable(&self) -> bool {
        self.flags & FLAG_EXECUTE != 0
    }

    /// The page-aligned range of memory the segment occupies.
    pub fn pages(&self) -> (u64, u64) {
        (
            self.vaddr & !(PAGE_SIZE - 1),
            (self.vaddr + self.mem_size).next_multiple_of(PAGE_SIZE),
        )
    }
}

/// A validated executable.
pub struct Elf<'a> {
    data: &'a [u8],
    entry: u64,
    ph_offset: usize,
    ph_count: usize,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

impl<'a> Elf<'a> {
    /// Parse and validate `data`, whose segments must all lie within
    /// `[lowest, highest)`.
    pub fn new(data: &'a [u8], lowest: u64, highest: u64) -> Result<Self, ElfError> {
        let t = ElfError::Truncated;
        if data.len() < HEADER_SIZE
            || data[..4] != ELF_MAGIC
            || data[4] != CLASS_64
            || data[5] != LITTLE_ENDIAN
        {
            return Err(ElfError::NotAArch64Elf);
        }
        if u16_at(data, 18).ok_or(t)? != MACHINE_AARCH64 {
            return Err(ElfError::NotAArch64Elf);
        }
        if u16_at(data, 16).ok_or(t)? != TYPE_EXECUTABLE {
            return Err(ElfError::NotExecutable);
        }
        let entry = u64_at(data, 24).ok_or(t)?;
        let ph_offset = usize::try_from(u64_at(data, 32).ok_or(t)?).map_err(|_| t)?;
        let ph_entry_size = u16_at(data, 54).ok_or(t)? as usize;
        let ph_count = u16_at(data, 56).ok_or(t)? as usize;
        if ph_entry_size != PROGRAM_HEADER_SIZE {
            return Err(ElfError::NotAArch64Elf);
        }
        let table_end = ph_count
            .checked_mul(PROGRAM_HEADER_SIZE)
            .and_then(|n| n.checked_add(ph_offset))
            .ok_or(t)?;
        if table_end > data.len() {
            return Err(t);
        }
        let elf = Elf {
            data,
            entry,
            ph_offset,
            ph_count,
        };

        // Check each segment, and that none share a page.
        let mut seen = [(0u64, 0u64); MAX_SEGMENTS];
        let mut count = 0;
        let mut entry_ok = false;
        for segment in elf.segments_unchecked() {
            let segment = segment?;
            if count == MAX_SEGMENTS {
                return Err(ElfError::BadSegmentCount);
            }
            if segment.is_writable() && segment.is_executable() {
                return Err(ElfError::WritableAndExecutable);
            }
            let end = segment
                .vaddr
                .checked_add(segment.mem_size)
                .ok_or(ElfError::BadAddress)?;
            if segment.vaddr < lowest
                || end > highest
                || end.checked_next_multiple_of(PAGE_SIZE).is_none()
            {
                return Err(ElfError::BadAddress);
            }
            let pages = segment.pages();
            if seen[..count]
                .iter()
                .any(|&(s, e)| pages.0 < e && s < pages.1)
            {
                return Err(ElfError::Overlap);
            }
            seen[count] = pages;
            count += 1;
            if segment.is_executable() && segment.vaddr <= entry && entry < end {
                entry_ok = true;
            }
        }
        if count == 0 {
            return Err(ElfError::BadSegmentCount);
        }
        if !entry_ok {
            return Err(ElfError::BadEntry);
        }
        Ok(elf)
    }

    pub fn entry(&self) -> u64 {
        self.entry
    }

    /// The loadable segments.
    pub fn segments(&self) -> impl Iterator<Item = Segment<'a>> + '_ {
        // `new` checked every segment, so none of these fail.
        self.segments_unchecked().filter_map(Result::ok)
    }

    fn segments_unchecked(&self) -> impl Iterator<Item = Result<Segment<'a>, ElfError>> + '_ {
        (0..self.ph_count).filter_map(move |i| {
            self.segment(self.ph_offset + i * PROGRAM_HEADER_SIZE)
                .transpose()
        })
    }

    /// The program header at `at`: `Ok(None)` if it isn't loadable.
    fn segment(&self, at: usize) -> Result<Option<Segment<'a>>, ElfError> {
        let t = ElfError::Truncated;
        let d = self.data;
        if u32_at(d, at).ok_or(t)? != PT_LOAD {
            return Ok(None);
        }
        let flags = u32_at(d, at + 4).ok_or(t)?;
        let offset = u64_at(d, at + 8).ok_or(t)?;
        let vaddr = u64_at(d, at + 16).ok_or(t)?;
        let file_size = u64_at(d, at + 32).ok_or(t)?;
        let mem_size = u64_at(d, at + 40).ok_or(t)?;
        if file_size > mem_size {
            return Err(ElfError::BadAddress);
        }
        let start = usize::try_from(offset).map_err(|_| t)?;
        let end = start
            .checked_add(usize::try_from(file_size).map_err(|_| t)?)
            .ok_or(t)?;
        let data = d.get(start..end).ok_or(t)?;
        Ok(Some(Segment {
            vaddr,
            mem_size,
            data,
            flags,
        }))
    }
}
