//! The Pi 4's PCIe controller (BCM2711, Broadcom "brcmstb"), and the VL805
//! USB host controller that is the only thing behind it.
//!
//! This is board set-up, like the UARTs: the kernel brings the link up, sets
//! up the VL805 and has the firmware load its firmware (which needs the
//! mailbox), then hands its xHCI registers to `init` like the Pi 5's USB
//! controllers, for the same user-space driver.
//!
//! The register sequence follows Linux's pcie-brcmstb driver for the
//! BCM2711, and the addresses the Pi 4's device tree gives:
//!
//! - registers at 0xFD50_0000;
//! - outbound: CPU 0x6_0000_0000 is PCIe memory 0xC000_0000, 1 GiB;
//! - inbound: PCIe address = physical address, for the first 3 GiB of RAM.
//!
//! QEMU has no model of this controller, so this is untested: every step
//! has a timeout, and any failure leaves USB off rather than stopping the
//! boot. The first register read is a probe, so a missing controller (as in
//! QEMU) is noticed rather than faulting.

use crate::addr::PhysAddr;
use crate::mailbox::Mailbox;
use crate::mmio;
use crate::timer::Deadline;

const BASE: PhysAddr = PhysAddr::new(0xFD50_0000);

// Root complex configuration (the root port's own config space) and
// controller registers, as Linux names them.
const RC_CFG_VENDOR_SPECIFIC_REG1: usize = 0x0188;
const VENDOR_REG1_ENDIAN_MODE_BAR2: u32 = 0xC;
const RC_CFG_PRIV1_ID_VAL3: usize = 0x043C;
const MISC_MISC_CTRL: usize = 0x4008;
const MISC_CTRL_SCB_ACCESS_EN: u32 = 1 << 12;
const MISC_CTRL_CFG_READ_UR_MODE: u32 = 1 << 13;
const MISC_CTRL_MAX_BURST_SIZE: u32 = 0x3 << 20;
const MISC_CTRL_SCB0_SIZE: u32 = 0x1F << 27;
const MISC_CPU_2_PCIE_MEM_WIN0_LO: usize = 0x400C;
const MISC_CPU_2_PCIE_MEM_WIN0_HI: usize = 0x4010;
const MISC_RC_BAR1_CONFIG_LO: usize = 0x402C;
const MISC_RC_BAR2_CONFIG_LO: usize = 0x4034;
const MISC_RC_BAR2_CONFIG_HI: usize = 0x4038;
const MISC_RC_BAR3_CONFIG_LO: usize = 0x403C;
const MISC_PCIE_STATUS: usize = 0x4068;
const STATUS_PHYLINKUP: u32 = 1 << 4;
const STATUS_DL_ACTIVE: u32 = 1 << 5;
const STATUS_PORT_RC: u32 = 1 << 7;
const MISC_REVISION: usize = 0x406C;
const MISC_CPU_2_PCIE_MEM_WIN0_BASE_LIMIT: usize = 0x4070;
const MISC_CPU_2_PCIE_MEM_WIN0_BASE_HI: usize = 0x4080;
const MISC_CPU_2_PCIE_MEM_WIN0_LIMIT_HI: usize = 0x4084;
const MISC_HARD_PCIE_HARD_DEBUG: usize = 0x4204;
const HARD_DEBUG_SERDES_IDDQ: u32 = 1 << 27;
const INTR2_CPU_CLEAR: usize = 0x4308;
const INTR2_CPU_MASK_SET: usize = 0x4310;
const EXT_CFG_DATA: usize = 0x8000;
const EXT_CFG_INDEX: usize = 0x9000;
const RGR1_SW_INIT_1: usize = 0x9210;
const SW_INIT_PERST: u32 = 1 << 0;
const SW_INIT_BRIDGE: u32 = 1 << 1;

/// The outbound window: where the CPU sees PCIe memory.
const WINDOW_CPU: u64 = 0x6_0000_0000;
const WINDOW_PCIE: u64 = 0xC000_0000;
const WINDOW_SIZE: u64 = 0x4000_0000;
/// Inbound: devices see RAM at its physical address, up to here (the
/// device tree's dma-ranges).
pub const DMA_LIMIT: PhysAddr = PhysAddr::new(0xC000_0000);

/// The VL805, and the firmware's "load the VL805's firmware" call.
const VL805_ID: u32 = 0x3483_1106;
const VL805_FIRMWARE_VERSION: usize = 0x50;
const TAG_NOTIFY_XHCI_RESET: u32 = 0x0003_0058;

/// Where we put the VL805's registers: the start of the window.
const XHCI_PCIE: u64 = WINDOW_PCIE;

/// The VL805's xHCI, ready for a driver.
pub struct Vl805 {
    /// Its registers, as the CPU sees them.
    pub base: PhysAddr,
    pub size: usize,
    pub firmware: u32,
}

fn reg(offset: usize) -> usize {
    BASE.to_virt().as_usize() + offset
}

fn read(offset: usize) -> u32 {
    mmio::read(reg(offset))
}

fn write(offset: usize, value: u32) {
    mmio::write(reg(offset), value)
}

fn modify(offset: usize, clear: u32, set: u32) {
    write(offset, (read(offset) & !clear) | set);
}

fn delay_us(us: u64) {
    let deadline = Deadline::after_us(us);
    while !deadline.expired() {
        core::hint::spin_loop();
    }
}

/// Select a device's config space (bus 1 and up) in the data window.
fn config_address(bus: u8, device: u8, register: usize) -> usize {
    write(EXT_CFG_INDEX, (bus as u32) << 20 | (device as u32) << 15);
    EXT_CFG_DATA + (register & 0xFFC)
}

fn config_read(bus: u8, device: u8, register: usize) -> u32 {
    if bus == 0 {
        read(register)
    } else {
        read(config_address(bus, device, register))
    }
}

fn config_write(bus: u8, device: u8, register: usize, value: u32) {
    if bus == 0 {
        write(register, value)
    } else {
        write(config_address(bus, device, register), value)
    }
}

/// Bring up the link and the VL805. Returns why not, if it didn't work.
pub fn start(mailbox: Mailbox) -> Result<Vl805, &'static str> {
    let revision = mmio::probe_read(reg(MISC_REVISION)).ok_or("no PCIe controller answers")?;
    if revision == 0 || revision == u32::MAX {
        return Err("no PCIe controller answers");
    }

    // Reset the bridge and assert PERST# (the device's reset), then take
    // the bridge out of reset and power up the SerDes.
    modify(RGR1_SW_INIT_1, 0, SW_INIT_BRIDGE | SW_INIT_PERST);
    delay_us(100);
    modify(RGR1_SW_INIT_1, SW_INIT_BRIDGE, 0);
    modify(MISC_HARD_PCIE_HARD_DEBUG, HARD_DEBUG_SERDES_IDDQ, 0);
    delay_us(100);

    // Inbound: devices reach RAM through BAR2, from PCIe address 0, 4 GiB
    // (a power of two covering the 3 GiB the device tree allows; the
    // driver keeps DMA below 3 GiB). Sizes are encoded as log2 - 15.
    const INBOUND_LOG2: u32 = 32;
    write(MISC_RC_BAR2_CONFIG_LO, INBOUND_LOG2 - 15);
    write(MISC_RC_BAR2_CONFIG_HI, 0);
    modify(
        MISC_MISC_CTRL,
        MISC_CTRL_MAX_BURST_SIZE | MISC_CTRL_SCB0_SIZE,
        MISC_CTRL_SCB_ACCESS_EN | MISC_CTRL_CFG_READ_UR_MODE | (INBOUND_LOG2 - 15) << 27,
    );
    modify(MISC_RC_BAR1_CONFIG_LO, 0x1F, 0);
    modify(MISC_RC_BAR3_CONFIG_LO, 0x1F, 0);

    // No interrupts from the controller: we poll.
    write(INTR2_CPU_CLEAR, u32::MAX);
    write(INTR2_CPU_MASK_SET, u32::MAX);

    // Outbound window 0: base and limit in MiB, split into low and high
    // parts.
    write(MISC_CPU_2_PCIE_MEM_WIN0_LO, WINDOW_PCIE as u32);
    write(MISC_CPU_2_PCIE_MEM_WIN0_HI, (WINDOW_PCIE >> 32) as u32);
    let base_mb = WINDOW_CPU >> 20;
    let limit_mb = (WINDOW_CPU + WINDOW_SIZE - 1) >> 20;
    write(
        MISC_CPU_2_PCIE_MEM_WIN0_BASE_LIMIT,
        ((limit_mb & 0xFFF) as u32) << 20 | ((base_mb & 0xFFF) as u32) << 4,
    );
    write(MISC_CPU_2_PCIE_MEM_WIN0_BASE_HI, (base_mb >> 12) as u32);
    write(MISC_CPU_2_PCIE_MEM_WIN0_LIMIT_HI, (limit_mb >> 12) as u32);

    // Release PERST# and wait for the link.
    modify(RGR1_SW_INIT_1, SW_INIT_PERST, 0);
    delay_us(100_000);
    let up = STATUS_PHYLINKUP | STATUS_DL_ACTIVE;
    let deadline = Deadline::after_us(100_000);
    while read(MISC_PCIE_STATUS) & up != up {
        if deadline.expired() {
            return Err("the PCIe link didn't come up");
        }
        delay_us(5_000);
    }
    if read(MISC_PCIE_STATUS) & STATUS_PORT_RC == 0 {
        return Err("the PCIe controller isn't a root complex");
    }

    // Present the root port as a PCI-to-PCI bridge, little-endian.
    modify(RC_CFG_PRIV1_ID_VAL3, 0xFF_FFFF, 0x06_0400);
    modify(RC_CFG_VENDOR_SPECIFIC_REG1, VENDOR_REG1_ENDIAN_MODE_BAR2, 0);

    // The root port: bus 1 behind it, the window's first MiB forwarded,
    // and memory and bus mastering on.
    config_write(0, 0, 0x18, 1 << 8 | 1 << 16);
    let window_mb = (XHCI_PCIE >> 20) as u32;
    config_write(0, 0, 0x20, window_mb << 4 | window_mb << 20);
    config_write(0, 0, 0x04, config_read(0, 0, 0x04) | 0x6);

    // The VL805 on bus 1.
    let id = config_read(1, 0, 0x00);
    if id != VL805_ID {
        return Err("the device on PCIe isn't a VL805");
    }
    // Size BAR0 (the xHCI registers), then place it.
    config_write(1, 0, 0x10, u32::MAX);
    let mask = config_read(1, 0, 0x10) & !0xF;
    let size = (!mask).wrapping_add(1) as usize;
    if size == 0 || size > 1 << 20 {
        return Err("the VL805's registers are an unexpected size");
    }
    let place_bar = || {
        config_write(1, 0, 0x10, XHCI_PCIE as u32);
        config_write(1, 0, 0x14, (XHCI_PCIE >> 32) as u32);
        config_write(1, 0, 0x04, config_read(1, 0, 0x04) | 0x6);
    };
    place_bar();

    // Have the firmware load the VL805's firmware, unless it is running
    // (Linux's rpi_firmware_init_vl805 does the same). The address is the
    // device's: bus 1, device 0, function 0.
    let mut firmware = config_read(1, 0, VL805_FIRMWARE_VERSION);
    if firmware == 0 {
        let mut address = [1u32 << 20];
        mailbox
            .property(TAG_NOTIFY_XHCI_RESET, &mut address)
            .ok_or("the firmware didn't answer the VL805 reset")?;
        delay_us(1_000);
        place_bar();
        firmware = config_read(1, 0, VL805_FIRMWARE_VERSION);
    }

    Ok(Vl805 {
        base: PhysAddr::new((WINDOW_CPU + (XHCI_PCIE - WINDOW_PCIE)) as usize),
        size,
        firmware,
    })
}
