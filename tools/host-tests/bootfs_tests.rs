//! Tests for the boot image format (bootfs/).

use pios_bootfs::*;

fn sample() -> Vec<u8> {
    let big = vec![0x5A; PAGE_SIZE + 1];
    build(&[
        ("init", b"init program"),
        ("hello", b"hi"),
        ("empty", b""),
        ("big", &big),
    ])
    .unwrap()
}

#[test]
fn round_trip() {
    let image = sample();
    assert!(image.len().is_multiple_of(PAGE_SIZE));
    let fs = BootFs::new(&image).unwrap();
    assert_eq!(fs.len(), 4);
    assert_eq!(BootFs::total_size(&image).unwrap(), image.len());
    let names: Vec<_> = fs.files().map(|f| f.name).collect();
    assert_eq!(names, ["big", "empty", "hello", "init"]);
    for file in fs.files() {
        assert!(file.offset.is_multiple_of(PAGE_SIZE));
    }
    assert_eq!(fs.find("init").unwrap().data, b"init program");
    assert_eq!(fs.find("hello").unwrap().data, b"hi");
    assert_eq!(fs.find("empty").unwrap().data, b"");
    assert_eq!(fs.find("big").unwrap().data.len(), PAGE_SIZE + 1);
    assert!(fs.find("nope").is_none());
    assert!(fs.find("").is_none());
    assert!(fs.find("zzz").is_none());
}

#[test]
fn an_empty_image_is_valid() {
    let image = build(&[]).unwrap();
    assert_eq!(image.len(), PAGE_SIZE);
    let fs = BootFs::new(&image).unwrap();
    assert!(fs.is_empty());
    assert!(fs.find("init").is_none());
}

#[test]
fn trailing_bytes_are_ignored() {
    let mut image = sample();
    let size = image.len();
    image.extend_from_slice(&[0xFF; 100]);
    assert_eq!(BootFs::new(&image).unwrap().as_bytes().len(), size);
}

#[test]
fn builder_refuses_bad_names() {
    assert_eq!(build(&[("", b"x")]), Err(Error::BadName));
    assert_eq!(build(&[("a\0b", b"x")]), Err(Error::BadName));
    let long = "x".repeat(NAME_MAX + 1);
    assert_eq!(build(&[(long.as_str(), b"x")]), Err(Error::BadName));
    let longest = "x".repeat(NAME_MAX);
    let image = build(&[(longest.as_str(), b"x")]).unwrap();
    assert_eq!(
        BootFs::new(&image).unwrap().find(&longest).unwrap().data,
        b"x"
    );
    assert_eq!(
        build(&[("a", b"1"), ("a", b"2")]),
        Err(Error::DuplicateName)
    );
}

fn put_u64(image: &mut [u8], at: usize, value: u64) {
    image[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

const FIRST_ENTRY: usize = HEADER_SIZE;

#[test]
fn refuses_bad_headers() {
    let image = sample();
    let mut bad = image.clone();
    bad[0] = b'X';
    assert_eq!(BootFs::new(&bad).err(), Some(Error::BadMagic));
    let mut bad = image.clone();
    bad[8] = 2;
    assert_eq!(BootFs::new(&bad).err(), Some(Error::UnsupportedVersion));
    let mut bad = image.clone();
    bad[24] = 1;
    assert_eq!(BootFs::new(&bad).err(), Some(Error::BadHeader));
    let mut bad = image.clone();
    put_u64(&mut bad, 16, image.len() as u64 + 1);
    assert_eq!(BootFs::new(&bad).err(), Some(Error::BadHeader));
    let mut bad = image.clone();
    put_u64(&mut bad, 16, image.len() as u64 + PAGE_SIZE as u64);
    assert_eq!(BootFs::new(&bad).err(), Some(Error::TooShort));
    let mut bad = image.clone();
    bad[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(BootFs::new(&bad).err(), Some(Error::BadDirectory));
    assert_eq!(
        BootFs::new(&image[..HEADER_SIZE - 1]).err(),
        Some(Error::TooShort)
    );
}

#[test]
fn refuses_bad_files() {
    let image = sample();
    let entry = |i: usize| FIRST_ENTRY + i * ENTRY_SIZE;
    // Misaligned.
    let mut bad = image.clone();
    put_u64(&mut bad, entry(0), PAGE_SIZE as u64 + 8);
    assert_eq!(BootFs::new(&bad).err(), Some(Error::BadFile));
    // Over the directory.
    let mut bad = image.clone();
    put_u64(&mut bad, entry(0), 0);
    assert_eq!(BootFs::new(&bad).err(), Some(Error::BadFile));
    // Overlapping the previous file.
    let fs = BootFs::new(&image).unwrap();
    let first = fs.files().next().unwrap();
    let mut bad = image.clone();
    put_u64(&mut bad, entry(1), first.offset as u64);
    assert_eq!(BootFs::new(&bad).err(), Some(Error::BadFile));
    // Off the end, or wrapping around.
    let mut bad = image.clone();
    put_u64(&mut bad, entry(3) + 8, image.len() as u64);
    assert_eq!(BootFs::new(&bad).err(), Some(Error::BadFile));
    let mut bad = image.clone();
    put_u64(&mut bad, entry(3) + 8, u64::MAX);
    assert_eq!(BootFs::new(&bad).err(), Some(Error::BadFile));
}

#[test]
fn refuses_bad_names() {
    let image = sample();
    let name = |i: usize| FIRST_ENTRY + i * ENTRY_SIZE + 16;
    // Empty.
    let mut bad = image.clone();
    bad[name(0)..name(0) + NAME_MAX].fill(0);
    assert_eq!(BootFs::new(&bad).err(), Some(Error::BadName));
    // Junk after the terminating zero.
    let mut bad = image.clone();
    bad[name(0) + NAME_MAX - 1] = b'x';
    assert_eq!(BootFs::new(&bad).err(), Some(Error::BadName));
    // Not UTF-8.
    let mut bad = image.clone();
    bad[name(0)] = 0xFF;
    assert_eq!(BootFs::new(&bad).err(), Some(Error::BadName));
    // Out of order ("big" -> "zig").
    let mut bad = image.clone();
    bad[name(0)] = b'z';
    assert_eq!(BootFs::new(&bad).err(), Some(Error::BadName));
    // A duplicate ("empty" -> "big\0\0").
    let mut bad = image.clone();
    bad[name(1)..name(1) + 5].copy_from_slice(b"big\0\0");
    assert_eq!(BootFs::new(&bad).err(), Some(Error::DuplicateName));
}

/// Whatever an image says, a reader either refuses it or hands out files
/// that lie within it, in order, never overlapping.
fn check_safe(image: &[u8]) {
    if let Ok(fs) = BootFs::new(image) {
        let mut end = HEADER_SIZE + fs.len() * ENTRY_SIZE;
        for file in fs.files() {
            assert!(file.offset >= end && file.offset.is_multiple_of(PAGE_SIZE));
            end = file.offset + file.data.len();
            assert!(end <= image.len());
            assert_eq!(fs.find(file.name).unwrap().offset, file.offset);
        }
    }
}

#[test]
fn every_truncation_is_refused() {
    let image = sample();
    for len in 0..image.len() {
        assert!(BootFs::new(&image[..len]).is_err(), "length {len}");
    }
}

#[test]
fn every_single_byte_corruption_is_safe() {
    let image = sample();
    let directory_end = HEADER_SIZE + 4 * ENTRY_SIZE;
    for at in 0..directory_end {
        for value in [0x00, 0x01, 0x10, 0x7F, 0x80, 0xFF] {
            let mut bad = image.clone();
            bad[at] = value;
            check_safe(&bad);
        }
    }
}
