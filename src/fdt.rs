//! A minimal reader for the flattened device tree (DTB) the firmware passes
//! us: for now just the header and the memory reservation list.
//!
//! Format: https://devicetree-specification.readthedocs.io (chapter 5). All
//! values are big-endian.

use crate::mmu;

const MAGIC: u32 = 0xD00D_FEED;
const HEADER_SIZE: usize = 40;
/// Refuse anything implausibly large rather than read far past it.
const MAX_SIZE: usize = 16 << 20;

pub struct Fdt {
    addr: usize,
    size: usize,
}

impl Fdt {
    /// The device tree at `addr`, if there is a valid one there.
    pub fn at(addr: usize) -> Option<Fdt> {
        // Only look at memory we have mapped.
        if addr == 0 || !addr.is_multiple_of(8) || addr + HEADER_SIZE > mmu::MAPPED_RAM_END {
            return None;
        }
        if read_be32(addr) != MAGIC {
            return None;
        }
        let size = read_be32(addr + 4) as usize;
        if !(HEADER_SIZE..=MAX_SIZE).contains(&size) || addr + size > mmu::MAPPED_RAM_END {
            return None;
        }
        Some(Fdt { addr, size })
    }

    pub fn addr(&self) -> usize {
        self.addr
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// The memory reservation block: (address, size) ranges that must not
    /// be used, for example because the firmware left something there.
    pub fn reservations(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        let offset = read_be32(self.addr + 16) as usize;
        let mut entry = self.addr + offset;
        core::iter::from_fn(move || {
            if entry + 16 > self.addr + self.size {
                return None;
            }
            let (addr, size) = (read_be64(entry), read_be64(entry + 8));
            if addr == 0 && size == 0 {
                return None;
            }
            entry += 16;
            Some((addr as usize, size as usize))
        })
    }
}

fn read_be32(addr: usize) -> u32 {
    // SAFETY: callers only read within a mapped, validated device tree.
    u32::from_be(unsafe { (addr as *const u32).read_unaligned() })
}

fn read_be64(addr: usize) -> u64 {
    // SAFETY: see `read_be32`.
    u64::from_be(unsafe { (addr as *const u64).read_unaligned() })
}
