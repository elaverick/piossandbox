//! Physical memory: finding the RAM, the frame allocator, and bringing up
//! the kernel's memory map.

use crate::addr::{LINEAR_MAP_SIZE, PAGE_SIZE, PhysAddr};
use crate::fdt::Fdt;
use crate::frames::{Frame, FrameAllocator};
use crate::mmu::{self, KernelMapPlan};
use crate::ranges::RangeSet;
use crate::sync::SpinLock;

static FRAMES: SpinLock<FrameAllocator<'static>> = SpinLock::new(FrameAllocator::new());

/// The boot map (boot.s) only covers the first GiB of RAM, so memory the
/// kernel must touch before its own map is up has to come from there.
const BOOT_MAP_RAM_END: PhysAddr = PhysAddr::new(1 << 30);

/// A frame of RAM, owned: it goes back to the allocator when dropped, so it
/// can't leak or be freed twice.
pub struct OwnedFrame(Frame);

impl OwnedFrame {
    /// A new frame, filled with zeros.
    pub fn allocate() -> Option<OwnedFrame> {
        allocate_zeroed_frame()
            .and_then(Frame::from_addr)
            .map(OwnedFrame)
    }

    pub fn addr(&self) -> PhysAddr {
        self.0.addr()
    }
}

impl Drop for OwnedFrame {
    fn drop(&mut self) {
        FRAMES.lock().free(self.0);
    }
}

/// Allocate a zeroed frame that the caller must give back with `free_frame`
/// (for page tables, which track their frames themselves).
pub fn allocate_zeroed_frame() -> Option<PhysAddr> {
    let frame = FRAMES.lock().allocate()?.addr();
    // SAFETY: the frame is ours, a whole page, and mapped (in the linear
    // map, or during boot in the first GiB, where the allocator's lowest
    // regions are).
    unsafe { core::ptr::write_bytes(frame.to_virt().as_ptr::<u8>(), 0, PAGE_SIZE) };
    Some(frame)
}

/// Give back a frame from `allocate_zeroed_frame`.
///
/// # Safety
///
/// Nothing may use the frame afterwards.
pub unsafe fn free_frame(frame: PhysAddr) {
    FRAMES
        .lock()
        .free(Frame::from_addr(frame).expect("frames are page-aligned"));
}

/// `count` zeroed, physically contiguous frames ending at or below `limit`,
/// for a device to use (DMA). They are also cleaned out of the caches, so
/// that a non-cacheable mapping of them sees the zeros, and no dirty cache
/// line can later be written back over what a device wrote.
pub fn allocate_dma(count: usize, limit: PhysAddr) -> Option<alloc::vec::Vec<OwnedFrame>> {
    let start = FRAMES
        .lock()
        .allocate_contiguous_below(count, PAGE_SIZE, limit)?;
    let virt = start.to_virt().as_usize();
    // SAFETY: the frames are ours, and in the linear map.
    unsafe { core::ptr::write_bytes(virt as *mut u8, 0, count * PAGE_SIZE) };
    crate::cache::clean_and_invalidate(virt, count * PAGE_SIZE);
    Some(
        (0..count)
            .map(|i| OwnedFrame(Frame::from_addr(start + i * PAGE_SIZE).expect("page-aligned")))
            .collect(),
    )
}

/// Allocate `size` bytes of contiguous RAM for the kernel's own use, for
/// good (the heap).
pub fn allocate_permanent(size: usize) -> Option<PhysAddr> {
    FRAMES
        .lock()
        .allocate_contiguous(size.div_ceil(PAGE_SIZE), PAGE_SIZE)
}

pub struct FrameStats {
    pub total: usize,
    pub free: usize,
}

pub fn frame_stats() -> FrameStats {
    let frames = FRAMES.lock();
    FrameStats {
        total: frames.total_frames(),
        free: frames.free_frames(),
    }
}

/// What the firmware told us about memory.
pub struct BootInfo<'a> {
    pub fdt: Option<Fdt<'a>>,
    /// Where the device tree itself is.
    pub fdt_range: Option<(PhysAddr, usize)>,
    /// The ARM's share of RAM below 1 GiB, from the mailbox.
    pub arm_memory: Option<(PhysAddr, usize)>,
    /// The GPU's memory, from the mailbox.
    pub vc_memory: Option<(PhysAddr, usize)>,
    pub framebuffer: Option<(PhysAddr, usize)>,
    pub devices: &'a [(PhysAddr, usize)],
}

/// What `init` found, for the boot banner.
pub struct MemoryReport {
    /// All RAM the kernel maps.
    pub ram: RangeSet<32>,
    pub from_device_tree: bool,
}

/// Find the RAM, start the frame allocator, and switch to the kernel's full
/// memory map.
pub fn init(boot: &BootInfo) -> Result<MemoryReport, &'static str> {
    let full = |_| "too many memory ranges";
    let range = |start: u64, size: u64| {
        let start = PhysAddr::new(start as usize);
        (start, start + size as usize)
    };

    // RAM, from the device tree's memory nodes, else the firmware.
    let mut ram = RangeSet::<32>::new();
    if let Some(fdt) = &boot.fdt {
        let root = fdt.root();
        let (a, s) = root.cells();
        for node in root.children().filter(|n| n.base_name() == "memory") {
            for (addr, size) in node.reg(a, s) {
                let (start, end) = range(addr, size);
                ram.add(start, end).map_err(full)?;
            }
        }
    }
    let from_device_tree = !ram.is_empty();
    if ram.is_empty() {
        let (base, size) = boot.arm_memory.ok_or("no memory information")?;
        ram.add(base, base + size).map_err(full)?;
    }
    // Only what the linear map can reach; never the GPU's memory; and never
    // "no-map" reservations (on the Pi 5, the secure firmware's memory).
    ram.remove(PhysAddr::new(LINEAR_MAP_SIZE), PhysAddr::new(usize::MAX))
        .map_err(full)?;
    if let Some((base, size)) = boot.vc_memory {
        ram.remove(base, base + size).map_err(full)?;
    }
    let mut reserved = RangeSet::<32>::new();
    if let Some(fdt) = &boot.fdt {
        for (addr, size) in fdt.reservations() {
            let (start, end) = range(addr, size);
            reserved.add(start, end).map_err(full)?;
        }
        if let Some(rm) = fdt.find("/reserved-memory") {
            let (a, s) = rm.cells();
            for child in rm.children() {
                for (addr, size) in child.reg(a, s) {
                    let (start, end) = range(addr, size);
                    reserved.add(start, end).map_err(full)?;
                    if child.property("no-map").is_some() {
                        ram.remove(start, end).map_err(full)?;
                    }
                }
            }
        }
    }
    ram.align_inward(PAGE_SIZE);

    // Usable RAM: not below the end of the kernel (firmware stubs, the
    // kernel and its stack), not the device tree, not reserved.
    let mut usable = ram;
    let (_, kernel_end) = mmu::kernel_image();
    usable.remove(PhysAddr::new(0), kernel_end).map_err(full)?;
    if let Some((start, size)) = boot.fdt_range {
        usable.remove(start, start + size).map_err(full)?;
    }
    for (start, end) in reserved.iter() {
        usable.remove(start, end).map_err(full)?;
    }
    if let Some((start, size)) = boot.framebuffer {
        usable.remove(start, start + size).map_err(full)?;
    }
    usable.align_inward(PAGE_SIZE);
    start_frame_allocator(&mut usable)?;

    let plan = KernelMapPlan {
        ram: &ram,
        devices: boot.devices,
        framebuffer: boot.framebuffer,
    };
    mmu::install_kernel_map(&plan).map_err(|_| "could not build the kernel's page tables")?;
    Ok(MemoryReport {
        ram,
        from_device_tree,
    })
}

/// Hand `usable` to the frame allocator, with its bitmaps carved from the
/// start of the lowest range that has room below `BOOT_MAP_RAM_END`.
/// Updates `usable` to exclude the bitmaps.
fn start_frame_allocator(usable: &mut RangeSet<32>) -> Result<(), &'static str> {
    let frames_in = |(start, end): (PhysAddr, PhysAddr)| (end - start) / PAGE_SIZE;
    let words: usize = usable
        .iter()
        .map(|r| FrameAllocator::bitmap_words(frames_in(r)))
        .sum();
    let bytes = (words * 8).next_multiple_of(PAGE_SIZE);
    let (start, _) = usable
        .iter()
        .find(|&(start, end)| end - start > bytes && start + bytes <= BOOT_MAP_RAM_END)
        .ok_or("no room for the frame allocator's bitmaps")?;
    usable
        .remove(start, start + bytes)
        .map_err(|_| "too many memory ranges")?;

    // SAFETY: this RAM is mapped (below BOOT_MAP_RAM_END), usable, now
    // removed from `usable` so the allocator will never hand it out, and
    // used for nothing else, for good.
    let mut bitmaps: &'static mut [u64] =
        unsafe { core::slice::from_raw_parts_mut(start.to_virt().as_ptr(), words) };
    let mut frames = FRAMES.lock();
    for region in usable.iter() {
        let (bitmap, rest) = bitmaps.split_at_mut(FrameAllocator::bitmap_words(frames_in(region)));
        bitmaps = rest;
        if frames
            .add_region(region.0, frames_in(region), bitmap)
            .is_err()
        {
            break; // more regions than the allocator tracks: use the first ones
        }
    }
    Ok(())
}
