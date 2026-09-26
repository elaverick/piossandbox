//! ARM GIC-400 interrupt controller (GICv2), used by both the Pi 4 and Pi 5.
//!
//! Interrupt IDs: 0-15 are software-generated (SGIs), 16-31 are per-core
//! (PPIs, e.g. the CPU's timers), and 32 upwards are shared peripherals
//! (SPIs; device-tree SPI n is ID n + 32).
//!
//! On real hardware the firmware leaves every interrupt in the non-secure
//! group for us; writing 1 to the enable bits below enables that group in
//! either the secure or non-secure view of the registers.

use crate::addr::PhysAddr;
use crate::mmio;

// Distributor registers.
const GICD_CTLR: usize = 0x000;
const GICD_TYPER: usize = 0x004;
const GICD_ISENABLER: usize = 0x100;
const GICD_ICENABLER: usize = 0x180;
const GICD_ICPENDR: usize = 0x280;
const GICD_IPRIORITYR: usize = 0x400;
const GICD_ITARGETSR: usize = 0x800;

// CPU interface registers.
const GICC_CTLR: usize = 0x000;
const GICC_PMR: usize = 0x004;
const GICC_BPR: usize = 0x008;
const GICC_IAR: usize = 0x00C;
const GICC_EOIR: usize = 0x010;

/// Priority given to every interrupt (lower is more urgent).
const PRIORITY: u32 = 0xA0;
/// Only interrupts more urgent than this are signalled.
const PRIORITY_MASK: u32 = 0xF0;

/// IDs from this value up mean "no interrupt pending" (spurious).
pub const SPURIOUS: u32 = 1020;

#[derive(Clone, Copy)]
pub struct Gic {
    distributor: usize,
    cpu: usize,
}

impl Gic {
    /// The GIC whose distributor and CPU interface registers are at these
    /// physical addresses.
    pub const fn new(distributor: PhysAddr, cpu: PhysAddr) -> Self {
        Gic {
            distributor: distributor.to_virt().as_usize(),
            cpu: cpu.to_virt().as_usize(),
        }
    }

    /// Reset the GIC to "everything disabled, routed to core 0" and turn it
    /// on. Returns the number of interrupt IDs it supports.
    pub fn init(&self) -> u32 {
        let d = self.distributor;
        let lines = 32 * ((mmio::read(d + GICD_TYPER) & 0x1F) + 1);

        mmio::write(d + GICD_CTLR, 0);
        for word in 0..(lines / 32) as usize {
            mmio::write(d + GICD_ICENABLER + word * 4, !0);
            mmio::write(d + GICD_ICPENDR + word * 4, !0);
        }
        // Route shared interrupts to this core. The first eight target
        // registers (SGIs and PPIs) are read-only and read back as this
        // core's own CPU interface bit.
        let this_core = mmio::read(d + GICD_ITARGETSR) & 0xFF;
        let targets = if this_core == 0 { 1 } else { this_core } * 0x0101_0101;
        // Four 8-bit fields per register.
        let priorities = PRIORITY * 0x0101_0101;
        for word in 0..(lines / 4) as usize {
            mmio::write(d + GICD_IPRIORITYR + word * 4, priorities);
            if word >= 8 {
                mmio::write(d + GICD_ITARGETSR + word * 4, targets);
            }
        }
        mmio::write(d + GICD_CTLR, 1);

        mmio::write(self.cpu + GICC_PMR, PRIORITY_MASK);
        mmio::write(self.cpu + GICC_BPR, 0);
        mmio::write(self.cpu + GICC_CTLR, 1);
        lines
    }

    pub fn enable(&self, id: u32) {
        let reg = self.distributor + GICD_ISENABLER + (id / 32) as usize * 4;
        mmio::write(reg, 1 << (id % 32));
    }

    pub fn disable(&self, id: u32) {
        let reg = self.distributor + GICD_ICENABLER + (id / 32) as usize * 4;
        mmio::write(reg, 1 << (id % 32));
    }

    /// Take the highest-priority pending interrupt. Returns the raw IAR
    /// value, to pass to `end`; its low 10 bits are the interrupt ID.
    pub fn acknowledge(&self) -> u32 {
        mmio::read(self.cpu + GICC_IAR)
    }

    /// Signal that handling of an acknowledged interrupt is complete.
    pub fn end(&self, iar: u32) {
        mmio::write(self.cpu + GICC_EOIR, iar);
    }
}
