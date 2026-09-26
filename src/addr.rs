//! Physical and virtual addresses, as distinct types so they can't be mixed
//! up, and the kernel's linear map between them.
//!
//! This file depends only on `core`, so the host unit tests compile it too.

use core::fmt;
use core::ops::{Add, Sub};

/// Where physical address 0 appears in the kernel's half of the address
/// space: physical address `p` is at virtual `KERNEL_BASE + p`.
pub const KERNEL_BASE: usize = 0xFFFF_FF80_0000_0000;

/// Physical addresses the linear map can cover: the lower half of the
/// kernel half (256 GiB, well beyond the Pi's RAM and peripherals). The rest
/// holds the thread stacks (see `stack.rs`).
pub const LINEAR_MAP_SIZE: usize = 1 << 38;

pub const PAGE_SIZE: usize = 4096;

/// A physical memory address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct PhysAddr(usize);

/// A virtual memory address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct VirtAddr(usize);

impl PhysAddr {
    pub const fn new(addr: usize) -> Self {
        PhysAddr(addr)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }

    /// Where the kernel sees this address, through its linear map.
    pub const fn to_virt(self) -> VirtAddr {
        assert!(
            self.0 < LINEAR_MAP_SIZE,
            "physical address beyond the linear map"
        );
        VirtAddr(KERNEL_BASE + self.0)
    }

    pub const fn is_page_aligned(self) -> bool {
        self.0.is_multiple_of(PAGE_SIZE)
    }

    pub const fn align_down(self, align: usize) -> Self {
        PhysAddr(self.0 & !(align - 1))
    }

    pub const fn align_up(self, align: usize) -> Self {
        PhysAddr(self.0.next_multiple_of(align))
    }
}

impl VirtAddr {
    pub const fn new(addr: usize) -> Self {
        VirtAddr(addr)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub fn from_ptr<T>(ptr: *const T) -> Self {
        VirtAddr(ptr as usize)
    }

    pub const fn as_ptr<T>(self) -> *mut T {
        self.0 as *mut T
    }

    /// The physical address behind a kernel linear-map address, or `None` if
    /// this isn't one.
    pub const fn to_phys(self) -> Option<PhysAddr> {
        if self.0 >= KERNEL_BASE && self.0 - KERNEL_BASE < LINEAR_MAP_SIZE {
            Some(PhysAddr(self.0 - KERNEL_BASE))
        } else {
            None
        }
    }

    pub const fn is_page_aligned(self) -> bool {
        self.0.is_multiple_of(PAGE_SIZE)
    }
}

impl Add<usize> for PhysAddr {
    type Output = PhysAddr;
    fn add(self, offset: usize) -> PhysAddr {
        PhysAddr(self.0 + offset)
    }
}

impl Sub<PhysAddr> for PhysAddr {
    type Output = usize;
    fn sub(self, other: PhysAddr) -> usize {
        self.0 - other.0
    }
}

impl Add<usize> for VirtAddr {
    type Output = VirtAddr;
    fn add(self, offset: usize) -> VirtAddr {
        VirtAddr(self.0 + offset)
    }
}

impl Sub<VirtAddr> for VirtAddr {
    type Output = usize;
    fn sub(self, other: VirtAddr) -> usize {
        self.0 - other.0
    }
}

impl fmt::Debug for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "PhysAddr({:#x})", self.0)
    }
}

impl fmt::Debug for VirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "VirtAddr({:#x})", self.0)
    }
}

impl fmt::LowerHex for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

impl fmt::LowerHex for VirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}
