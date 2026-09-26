//! Host unit tests for the parts of the kernel that are plain logic: they
//! are compiled in unchanged from src/. They also test the crates shared
//! with user space (`abi/`, `bootfs/`). Run them with `make test-host`. (The
//! kernel crate is built for the Pi, so these live in their own crate, built
//! for the host.)

#[path = "../../src/addr.rs"]
pub mod addr;
#[path = "../../src/allocator.rs"]
pub mod allocator;
#[path = "../../src/elf.rs"]
pub mod elf;
#[path = "../../src/fdt.rs"]
pub mod fdt;
#[path = "../../src/frames.rs"]
pub mod frames;
#[path = "../../src/paging.rs"]
pub mod paging;
#[path = "../../src/ranges.rs"]
pub mod ranges;

#[cfg(test)]
mod abi_tests;
#[cfg(test)]
mod allocator_tests;
#[cfg(test)]
mod bootfs_tests;
#[cfg(test)]
mod elf_tests;
#[cfg(test)]
mod fdt_tests;
#[cfg(test)]
mod frames_tests;
#[cfg(test)]
mod paging_tests;
#[cfg(test)]
mod ranges_tests;
