//! The boot image: the programs needed to start the system, built into the
//! kernel (see docs/design.md for why, and `pios-bootfs` for the format).
//!
//! The kernel itself only looks up `init` here (and, for its self-test, the
//! test programs); `init` gets the whole image and starts everything else.

use pios_bootfs::BootFs;

/// Page-aligned, so the image can be mapped into `init` without sharing a
/// page with anything else. (The builder pads it to whole pages.)
#[repr(C, align(4096))]
struct PageAligned<T: ?Sized>(T);

static IMAGE: &PageAligned<[u8]> =
    &PageAligned(*include_bytes!(concat!(env!("OUT_DIR"), "/boot.img")));

/// The boot image, checked.
pub fn image() -> BootFs<'static> {
    BootFs::new(&IMAGE.0).expect("the build wrote a valid boot image")
}

/// The file called `name`.
pub fn program(name: &str) -> Option<&'static [u8]> {
    image().find(name).map(|file| file.data)
}
