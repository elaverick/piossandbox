//! The pios boot image: a read-only archive of the programs needed to start
//! the system, carried in the kernel image. The build writes it (with the
//! `builder` feature), and the kernel and `init` read it. See the design
//! notes in docs/design.md.
//!
//! Layout, all little-endian:
//!
//! | Part | Contents |
//! | --- | --- |
//! | header, 32 bytes | magic `piosboot`, version (u32), file count (u32), total size (u64), 8 zero bytes |
//! | directory, 64 bytes per file | offset (u64) and size (u64) of the file, its name (48 bytes, UTF-8, zero-padded) |
//! | files | each starting on a page boundary |
//!
//! Directory entries are in strictly increasing name order (so names are
//! unique and can be binary searched), and files are laid out in the same
//! order without overlapping. The total size is a whole number of pages.
//!
//! Readers check all of this before handing out anything, so a corrupt
//! image is refused rather than misread.

#![no_std]
#![deny(unsafe_code)]

#[cfg(feature = "builder")]
extern crate alloc;

use core::cmp::Ordering;

pub const MAGIC: [u8; 8] = *b"piosboot";
pub const VERSION: u32 = 1;
/// Files start, and the image ends, on multiples of this.
pub const PAGE_SIZE: usize = 4096;
pub const HEADER_SIZE: usize = 32;
pub const ENTRY_SIZE: usize = 64;
/// The longest file name, in bytes.
pub const NAME_MAX: usize = 48;

/// Why an image (or, for the builder, a file list) was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Shorter than its header, or than the size the header gives.
    TooShort,
    BadMagic,
    UnsupportedVersion,
    /// The header's reserved bytes aren't zero, or its total size isn't a
    /// whole number of pages.
    BadHeader,
    /// The directory doesn't fit in the image.
    BadDirectory,
    /// A file is misaligned, overlaps another, or runs off the end.
    BadFile,
    /// A name is empty, too long, not UTF-8, or out of order.
    BadName,
    /// Two files have the same name.
    DuplicateName,
}

/// A checked boot image.
#[derive(Clone, Copy)]
pub struct BootFs<'a> {
    /// The image, up to its total size.
    data: &'a [u8],
    count: usize,
}

/// One file in the image.
#[derive(Clone, Copy, Debug)]
pub struct File<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
    /// Where `data` starts in the image: a multiple of `PAGE_SIZE`.
    pub offset: usize,
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

/// The name stored in a directory entry's name field: everything before
/// the first zero byte, which must be all that follows.
fn parse_name(field: &[u8]) -> Result<&str, Error> {
    let len = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    if len == 0 || field[len..].iter().any(|&b| b != 0) {
        return Err(Error::BadName);
    }
    core::str::from_utf8(&field[..len]).map_err(|_| Error::BadName)
}

impl<'a> BootFs<'a> {
    /// The total size of the image starting with `header` (at least
    /// `HEADER_SIZE` bytes), so a reader that only has a pointer to the
    /// image knows how much of it there is. Checks the header only.
    pub fn total_size(header: &[u8]) -> Result<usize, Error> {
        if header.len() < HEADER_SIZE {
            return Err(Error::TooShort);
        }
        if header[0..8] != MAGIC {
            return Err(Error::BadMagic);
        }
        if u32_at(header, 8) != VERSION {
            return Err(Error::UnsupportedVersion);
        }
        let total = usize::try_from(u64_at(header, 16)).map_err(|_| Error::BadHeader)?;
        if u64_at(header, 24) != 0 || total < HEADER_SIZE || !total.is_multiple_of(PAGE_SIZE) {
            return Err(Error::BadHeader);
        }
        Ok(total)
    }

    /// Check the image in `data` (which may run on past its end).
    pub fn new(data: &'a [u8]) -> Result<BootFs<'a>, Error> {
        let total = Self::total_size(data)?;
        let data = data.get(..total).ok_or(Error::TooShort)?;
        let count = u32_at(data, 12) as usize;
        let directory_end = count
            .checked_mul(ENTRY_SIZE)
            .and_then(|size| size.checked_add(HEADER_SIZE))
            .filter(|&end| end <= total)
            .ok_or(Error::BadDirectory)?;

        let image = BootFs { data, count };
        let mut previous: Option<&str> = None;
        let mut free_from = directory_end;
        for i in 0..count {
            let entry = image.entry(i);
            let name = parse_name(&entry[16..64])?;
            match previous.map(|p| p.cmp(name)) {
                Some(Ordering::Equal) => return Err(Error::DuplicateName),
                Some(Ordering::Greater) => return Err(Error::BadName),
                _ => {}
            }
            previous = Some(name);

            let offset = usize::try_from(u64_at(entry, 0)).map_err(|_| Error::BadFile)?;
            let size = usize::try_from(u64_at(entry, 8)).map_err(|_| Error::BadFile)?;
            let end = offset.checked_add(size).ok_or(Error::BadFile)?;
            if !offset.is_multiple_of(PAGE_SIZE) || offset < free_from || end > total {
                return Err(Error::BadFile);
            }
            free_from = end;
        }
        Ok(image)
    }

    fn entry(&self, i: usize) -> &'a [u8] {
        let at = HEADER_SIZE + i * ENTRY_SIZE;
        &self.data[at..at + ENTRY_SIZE]
    }

    /// The `i`th file (`new` checked every entry, so this can't fail).
    fn file(&self, i: usize) -> File<'a> {
        let entry = self.entry(i);
        let offset = u64_at(entry, 0) as usize;
        let size = u64_at(entry, 8) as usize;
        File {
            name: parse_name(&entry[16..64]).expect("checked by new"),
            data: &self.data[offset..offset + size],
            offset,
        }
    }

    /// The whole image.
    pub fn as_bytes(&self) -> &'a [u8] {
        self.data
    }

    /// Number of files.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The files, in name order.
    pub fn files(&self) -> impl Iterator<Item = File<'a>> + use<'a> {
        let image = *self;
        (0..self.count).map(move |i| image.file(i))
    }

    /// The file called `name`.
    pub fn find(&self, name: &str) -> Option<File<'a>> {
        let (mut low, mut high) = (0, self.count);
        while low < high {
            let middle = low + (high - low) / 2;
            let file = self.file(middle);
            match file.name.cmp(name) {
                Ordering::Less => low = middle + 1,
                Ordering::Greater => high = middle,
                Ordering::Equal => return Some(file),
            }
        }
        None
    }
}

/// Write an image holding `files` (name, contents), in any order.
#[cfg(feature = "builder")]
pub fn build(files: &[(&str, &[u8])]) -> Result<alloc::vec::Vec<u8>, Error> {
    use alloc::vec::Vec;

    let mut files: Vec<(&str, &[u8])> = files.to_vec();
    files.sort_by(|a, b| a.0.cmp(b.0));
    for pair in files.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(Error::DuplicateName);
        }
    }
    let count = u32::try_from(files.len()).map_err(|_| Error::BadDirectory)?;
    let page_up = |n: usize| n.next_multiple_of(PAGE_SIZE);

    let mut offset = page_up(HEADER_SIZE + files.len() * ENTRY_SIZE);
    let mut directory = Vec::new();
    for (name, data) in &files {
        if name.is_empty() || name.len() > NAME_MAX || name.contains('\0') {
            return Err(Error::BadName);
        }
        directory.extend_from_slice(&(offset as u64).to_le_bytes());
        directory.extend_from_slice(&(data.len() as u64).to_le_bytes());
        let mut field = [0u8; NAME_MAX];
        field[..name.len()].copy_from_slice(name.as_bytes());
        directory.extend_from_slice(&field);
        offset = page_up(offset + data.len());
    }
    let total = offset;

    let mut image = Vec::with_capacity(total);
    image.extend_from_slice(&MAGIC);
    image.extend_from_slice(&VERSION.to_le_bytes());
    image.extend_from_slice(&count.to_le_bytes());
    image.extend_from_slice(&(total as u64).to_le_bytes());
    image.extend_from_slice(&[0; 8]);
    image.extend_from_slice(&directory);
    for (_, data) in &files {
        image.resize(page_up(image.len()), 0);
        image.extend_from_slice(data);
    }
    image.resize(total, 0);
    Ok(image)
}
