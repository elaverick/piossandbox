//! Board detection and board-specific bring-up.
//!
//! One kernel image runs on both the Raspberry Pi 4 (BCM2711) and the
//! Raspberry Pi 5 (BCM2712). We tell them apart by their CPU cores, which
//! works before we have any other way to talk to the hardware.

use crate::addr::PhysAddr;
use crate::cpu;
use crate::gpio::{Function, Gpio, Pull};
use crate::mailbox::{self, Mailbox};
use crate::mmio;
use crate::uart::{self, Pl011};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// Raspberry Pi 4, 400 and Compute Module 4 (BCM2711, Cortex-A72).
    Pi4,
    /// Raspberry Pi 5, 500 and Compute Module 5 (BCM2712, Cortex-A76).
    Pi5,
}

impl Model {
    pub fn detect() -> Option<Model> {
        match cpu::id() {
            (cpu::IMPLEMENTER_ARM, cpu::PART_CORTEX_A72, ..) => Some(Model::Pi4),
            (cpu::IMPLEMENTER_ARM, cpu::PART_CORTEX_A76, ..) => Some(Model::Pi5),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Model::Pi4 => "Raspberry Pi 4",
            Model::Pi5 => "Raspberry Pi 5",
        }
    }

    pub fn mailbox(self) -> Mailbox {
        match self {
            Model::Pi4 => Mailbox::new(pi4::MBOX_BASE),
            Model::Pi5 => Mailbox::new(pi5::MBOX_BASE),
        }
    }

    /// Physical ranges holding peripherals, for the kernel map.
    pub fn devices(self) -> &'static [(PhysAddr, usize)] {
        match self {
            Model::Pi4 => pi4::DEVICES,
            Model::Pi5 => pi5::DEVICES,
        }
    }

    /// The GIC-400's distributor and CPU interface addresses.
    pub fn gic(self) -> (PhysAddr, PhysAddr) {
        match self {
            Model::Pi4 => (pi4::GICD_BASE, pi4::GICC_BASE),
            Model::Pi5 => (pi5::GICD_BASE, pi5::GICC_BASE),
        }
    }
}

/// A UART that was added to the console, for the boot banner.
pub struct ConsoleUart {
    pub name: &'static str,
    /// The reference clock we programmed the UART with, or `None` if we kept
    /// the firmware's settings.
    pub clock_hz: Option<u32>,
    /// Its interrupt ID, or `None` if input is polled.
    pub irq: Option<u32>,
}

/// Set up the console UARTs for `model` and register them with the console.
pub fn init_console(model: Model) -> [Option<ConsoleUart>; 2] {
    let uarts = match model {
        Model::Pi4 => pi4::init_console(),
        Model::Pi5 => pi5::init_console(),
    };
    for (uart, info) in uarts.iter().flatten() {
        crate::console::add(*uart, info.irq);
    }
    uarts.map(|u| u.map(|(_, info)| info))
}

mod pi4 {
    use super::*;

    /// Base of the peripheral window as seen by the ARM cores in the default
    /// "low peripheral" mode. (The Pi 2/3 use 0x3F00_0000, the Pi 1/Zero
    /// 0x2000_0000.)
    const MMIO_BASE: PhysAddr = PhysAddr::new(0xFE00_0000);
    /// For the kernel map: everything from the main peripherals up to the
    /// GIC.
    pub const DEVICES: &[(PhysAddr, usize)] = &[(PhysAddr::new(0xFC00_0000), 0x0400_0000)];
    const GPIO_BASE: PhysAddr = PhysAddr::new(MMIO_BASE.as_usize() + 0x20_0000);
    const UART0_BASE: PhysAddr = PhysAddr::new(MMIO_BASE.as_usize() + 0x20_1000);
    pub const MBOX_BASE: PhysAddr = PhysAddr::new(MMIO_BASE.as_usize() + 0xB880);
    /// The GIC-400 sits in the "ARM local" block just above the peripherals.
    pub const GICD_BASE: PhysAddr = PhysAddr::new(0xFF84_1000);
    pub const GICC_BASE: PhysAddr = PhysAddr::new(0xFF84_2000);
    /// UART0's interrupt: SPI 121 in the device tree.
    const UART0_IRQ: u32 = 32 + 121;

    /// Clock we ask the firmware to run the UART at (as the OSDev tutorial
    /// does).
    const REQUESTED_CLOCK_HZ: u32 = 3_000_000;
    /// The firmware's default UART clock, used if the mailbox call fails.
    const DEFAULT_CLOCK_HZ: u32 = 48_000_000;

    pub fn init_console() -> [Option<(Pl011, ConsoleUart)>; 2] {
        // Route GPIO 14/15 to UART0 (ALT0) with no pull resistors. The
        // firmware gives these pins to the mini UART by default.
        let gpio = Gpio::new(GPIO_BASE);
        for pin in [14, 15] {
            gpio.set_function(pin, Function::Alt0);
            gpio.set_pull(pin, Pull::None);
        }

        // Ask the firmware to set the UART clock, and use whatever rate it
        // actually reports back to compute the divisor.
        let mut clock = [mailbox::CLOCK_UART, REQUESTED_CLOCK_HZ, 0];
        let clock_hz =
            match Mailbox::new(MBOX_BASE).property(mailbox::TAG_SET_CLOCK_RATE, &mut clock) {
                Some(()) if clock[1] != 0 => clock[1],
                _ => DEFAULT_CLOCK_HZ,
            };

        let uart0 = Pl011::new(UART0_BASE);
        uart0.init(clock_hz, uart::BAUD_RATE);
        let info = ConsoleUart {
            name: "UART0 on GPIO 14/15",
            clock_hz: Some(clock_hz),
            irq: Some(UART0_IRQ),
        };
        [Some((uart0, info)), None]
    }
}

mod pi5 {
    use super::*;

    // Addresses from the BCM2712 device tree (bcm2712-rpi-5-b.dtb). The SoC
    // peripherals sit at 0x10_0000_0000 + their bus address.

    /// The PL011 behind the 3-pin "UART" connector between the HDMI ports.
    /// For the kernel map: the PCIe controllers, SoC peripherals and GIC,
    /// and RP1 through PCIe.
    pub const DEVICES: &[(PhysAddr, usize)] = &[
        (PhysAddr::new(0x10_0000_0000), 0x8000_0000),
        (PhysAddr::new(0x1F_0000_0000), 0x4000_0000),
    ];
    const DEBUG_UART_BASE: PhysAddr = PhysAddr::new(0x10_7D00_1000);
    /// Its fixed 9.216 MHz reference clock ("clk-uart" in the device tree).
    const DEBUG_UART_CLOCK_HZ: u32 = 9_216_000;
    pub const MBOX_BASE: PhysAddr = PhysAddr::new(0x10_7C01_3880);
    pub const GICD_BASE: PhysAddr = PhysAddr::new(0x10_7FFF_9000);
    pub const GICC_BASE: PhysAddr = PhysAddr::new(0x10_7FFF_A000);
    /// The debug UART's interrupt: SPI 121 in the device tree.
    const DEBUG_UART_IRQ: u32 = 32 + 121;

    /// The PCIe controller that connects the BCM2712 to RP1.
    const RP1_PCIE_BASE: PhysAddr = PhysAddr::new(0x10_0012_0000);
    const PCIE_MISC_PCIE_STATUS: usize = 0x4068;
    const PCIE_STATUS_PHYLINKUP: u32 = 1 << 4;
    const PCIE_STATUS_DL_ACTIVE: u32 = 1 << 5;

    /// RP1's peripherals appear through PCIe at 0x1F_0000_0000; its UART0
    /// (GPIO 14/15, header pins 8 and 10) is at RP1 offset 0x30000.
    const RP1_UART0_BASE: PhysAddr = PhysAddr::new(0x1F_0003_0000);

    pub fn init_console() -> [Option<(Pl011, ConsoleUart)>; 2] {
        let debug = Pl011::new(DEBUG_UART_BASE);
        debug.init(DEBUG_UART_CLOCK_HZ, uart::BAUD_RATE);
        let debug_info = ConsoleUart {
            name: "debug UART connector",
            clock_hz: Some(DEBUG_UART_CLOCK_HZ),
            irq: Some(DEBUG_UART_IRQ),
        };

        // RP1 is only reachable if the firmware left the PCIe link up, which
        // it does with `enable_rp1_uart=1` in config.txt. That option also
        // makes the firmware set up UART0 and its pins at 115200 baud; RP1's
        // clocks are not ours to reprogram yet, so we keep its settings.
        let rp1 = if rp1_link_up() {
            let uart0 = Pl011::new(RP1_UART0_BASE);
            if uart0.is_configured() {
                uart0.enable();
                // RP1's interrupts reach the GIC as PCIe MSIs, which we
                // don't set up yet, so this UART is polled.
                let info = ConsoleUart {
                    name: "RP1 UART0 on GPIO 14/15",
                    clock_hz: None,
                    irq: None,
                };
                Some((uart0, info))
            } else {
                None
            }
        } else {
            None
        };

        [Some((debug, debug_info)), rp1]
    }

    fn rp1_link_up() -> bool {
        let status = mmio::read(RP1_PCIE_BASE.to_virt().as_usize() + PCIE_MISC_PCIE_STATUS);
        let up = PCIE_STATUS_PHYLINKUP | PCIE_STATUS_DL_ACTIVE;
        status & up == up
    }
}
