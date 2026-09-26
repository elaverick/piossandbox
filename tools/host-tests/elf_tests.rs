//! Tests for src/elf.rs.

use crate::elf::*;

const LOW: u64 = 0x1000;
const HIGH: u64 = 1 << 39;

/// Build a minimal AArch64 executable from (vaddr, mem_size, data, flags)
/// segments.
fn make_elf(entry: u64, segments: &[(u64, u64, &[u8], u32)]) -> Vec<u8> {
    let ph_offset = 64usize;
    let data_start = ph_offset + 56 * segments.len();
    let mut out = vec![0u8; data_start];
    out[..4].copy_from_slice(&[0x7F, b'E', b'L', b'F']);
    out[4] = 2; // 64-bit
    out[5] = 1; // little-endian
    out[6] = 1; // version
    out[16..18].copy_from_slice(&2u16.to_le_bytes()); // executable
    out[18..20].copy_from_slice(&183u16.to_le_bytes()); // AArch64
    out[20..24].copy_from_slice(&1u32.to_le_bytes());
    out[24..32].copy_from_slice(&entry.to_le_bytes());
    out[32..40].copy_from_slice(&(ph_offset as u64).to_le_bytes());
    out[52..54].copy_from_slice(&64u16.to_le_bytes());
    out[54..56].copy_from_slice(&56u16.to_le_bytes());
    out[56..58].copy_from_slice(&(segments.len() as u16).to_le_bytes());
    for (i, &(vaddr, mem_size, data, flags)) in segments.iter().enumerate() {
        let offset = out.len() as u64;
        out.extend_from_slice(data);
        let ph = &mut out[ph_offset + 56 * i..ph_offset + 56 * (i + 1)];
        ph[0..4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        ph[4..8].copy_from_slice(&flags.to_le_bytes());
        ph[8..16].copy_from_slice(&offset.to_le_bytes());
        ph[16..24].copy_from_slice(&vaddr.to_le_bytes());
        ph[24..32].copy_from_slice(&vaddr.to_le_bytes());
        ph[32..40].copy_from_slice(&(data.len() as u64).to_le_bytes());
        ph[40..48].copy_from_slice(&mem_size.to_le_bytes());
    }
    out
}

const RX: u32 = FLAG_READ | FLAG_EXECUTE;
const R: u32 = FLAG_READ;
const RW: u32 = FLAG_READ | FLAG_WRITE;

fn typical() -> Vec<u8> {
    make_elf(0x40_0000, &[
        (0x40_0000, 8, &[0xAA; 8], RX),
        (0x40_1000, 4, &[0xBB; 4], R),
        (0x40_2000, 0x2000, &[0xCC; 16], RW), // data plus zero-filled bss
    ])
}

#[test]
fn parses_a_typical_program() {
    let bytes = typical();
    let elf = Elf::new(&bytes, LOW, HIGH).unwrap();
    assert_eq!(elf.entry(), 0x40_0000);
    let segments: Vec<_> = elf.segments().collect();
    assert_eq!(segments.len(), 3);
    assert_eq!(segments[0].data, &[0xAA; 8]);
    assert!(segments[0].is_executable() && !segments[0].is_writable());
    assert_eq!(segments[2].mem_size, 0x2000);
    assert_eq!(segments[2].pages(), (0x40_2000, 0x40_4000));
}

#[test]
fn refuses_writable_and_executable_segments() {
    let bytes = make_elf(0x40_0000, &[(0x40_0000, 8, &[0; 8], RW | FLAG_EXECUTE)]);
    assert_eq!(Elf::new(&bytes, LOW, HIGH).err(), Some(ElfError::WritableAndExecutable));
}

#[test]
fn refuses_segments_outside_user_space() {
    for vaddr in [0u64, 0xFFFF_FF80_0008_0000, HIGH - 4] {
        let bytes = make_elf(vaddr, &[(vaddr, 8, &[0; 8], RX)]);
        assert_eq!(Elf::new(&bytes, LOW, HIGH).err(), Some(ElfError::BadAddress), "{vaddr:#x}");
    }
    let bytes = make_elf(0x40_0000, &[(0x40_0000, u64::MAX, &[0; 8], RX)]);
    assert_eq!(Elf::new(&bytes, LOW, HIGH).err(), Some(ElfError::BadAddress), "overflowing size");
}

#[test]
fn refuses_segments_sharing_a_page() {
    let bytes = make_elf(0x40_0000, &[(0x40_0000, 8, &[0; 8], RX), (0x40_0800, 8, &[0; 8], RW)]);
    assert_eq!(Elf::new(&bytes, LOW, HIGH).err(), Some(ElfError::Overlap));
}

#[test]
fn entry_must_be_in_executable_code() {
    let bytes = make_elf(0x40_1000, &[(0x40_0000, 8, &[0; 8], RX), (0x40_1000, 8, &[0; 8], R)]);
    assert_eq!(Elf::new(&bytes, LOW, HIGH).err(), Some(ElfError::BadEntry));
    let bytes = make_elf(0x50_0000, &[(0x40_0000, 8, &[0; 8], RX)]);
    assert_eq!(Elf::new(&bytes, LOW, HIGH).err(), Some(ElfError::BadEntry));
}

#[test]
fn refuses_other_kinds_of_file() {
    let good = typical();
    let mut wrong_machine = good.clone();
    wrong_machine[18] = 62; // x86-64
    assert_eq!(Elf::new(&wrong_machine, LOW, HIGH).err(), Some(ElfError::NotAArch64Elf));
    let mut shared_library = good.clone();
    shared_library[16] = 3;
    assert_eq!(Elf::new(&shared_library, LOW, HIGH).err(), Some(ElfError::NotExecutable));
    let mut thirty_two_bit = good.clone();
    thirty_two_bit[4] = 1;
    assert_eq!(Elf::new(&thirty_two_bit, LOW, HIGH).err(), Some(ElfError::NotAArch64Elf));
    assert_eq!(Elf::new(b"#!/bin/sh\n", LOW, HIGH).err(), Some(ElfError::NotAArch64Elf));
    let empty = make_elf(0x40_0000, &[]);
    assert_eq!(Elf::new(&empty, LOW, HIGH).err(), Some(ElfError::BadSegmentCount));
}

#[test]
fn refuses_file_data_larger_than_memory_size() {
    let bytes = make_elf(0x40_0000, &[(0x40_0000, 4, &[0; 8], RX)]);
    assert_eq!(Elf::new(&bytes, LOW, HIGH).err(), Some(ElfError::BadAddress));
}

#[test]
fn every_truncation_and_single_byte_corruption_is_refused_or_harmless() {
    let good = typical();
    for len in 0..good.len() {
        if let Ok(elf) = Elf::new(&good[..len], LOW, HIGH) {
            let _ = elf.segments().count();
        }
    }
    for pos in 0..good.len() {
        for flip in [0x01u8, 0x80, 0xFF] {
            let mut bytes = good.clone();
            bytes[pos] ^= flip;
            if let Ok(elf) = Elf::new(&bytes, LOW, HIGH) {
                // Whatever got through still satisfies the guarantees.
                for s in elf.segments() {
                    assert!(!(s.is_writable() && s.is_executable()));
                    assert!(s.vaddr >= LOW && s.vaddr + s.mem_size <= HIGH);
                    assert!(s.data.len() as u64 <= s.mem_size);
                }
            }
        }
    }
}

/// The real user programs, if they have been built.
#[test]
fn pios_user_programs() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../user/target/aarch64-unknown-none/release");
    let mut checked = 0;
    for name in ["hello", "usertest", "crashtest"] {
        let Ok(bytes) = std::fs::read(format!("{dir}/{name}")) else { continue };
        let elf = Elf::new(&bytes, LOW, HIGH).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(elf.entry(), 0x40_0000, "{name}: _start comes first");
        assert!(elf.segments().any(|s| s.is_executable()));
        checked += 1;
    }
    eprintln!("checked {checked} user programs");
}
