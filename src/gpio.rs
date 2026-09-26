//! BCM2711 (Raspberry Pi 4) GPIO: just enough to route pins to peripherals.
//!
//! On the Pi 5 the 40-pin header belongs to the RP1 chip, whose GPIO block
//! works differently; the firmware sets up the pins we need there.

use crate::mmio;

/// Function select registers, 10 pins per register, 3 bits per pin.
const GPFSEL0: usize = 0x00;
/// Pull-up/down control, 16 pins per register, 2 bits per pin. These are new
/// on the BCM2711; the GPPUD/GPPUDCLK dance used on earlier Pis (and in the
/// OSDev tutorial) does nothing on a Pi 4.
const GPIO_PUP_PDN_CNTRL_REG0: usize = 0xE4;

#[derive(Clone, Copy)]
#[repr(u32)]
#[allow(dead_code)]
pub enum Function {
    Input = 0b000,
    Output = 0b001,
    Alt0 = 0b100,
    Alt1 = 0b101,
    Alt2 = 0b110,
    Alt3 = 0b111,
    Alt4 = 0b011,
    Alt5 = 0b010,
}

#[derive(Clone, Copy)]
#[repr(u32)]
#[allow(dead_code)]
pub enum Pull {
    None = 0b00,
    Up = 0b01,
    Down = 0b10,
}

/// The BCM2711 GPIO controller at `base`.
#[derive(Clone, Copy)]
pub struct Gpio {
    base: usize,
}

impl Gpio {
    pub const fn new(base: usize) -> Self {
        Gpio { base }
    }

    /// Select the function of a GPIO pin (0-57).
    pub fn set_function(&self, pin: u32, function: Function) {
        let reg = self.base + GPFSEL0 + (pin / 10) as usize * 4;
        let shift = (pin % 10) * 3;
        let mut value = mmio::read(reg);
        value &= !(0b111 << shift);
        value |= (function as u32) << shift;
        mmio::write(reg, value);
    }

    /// Configure the pull-up/pull-down resistor of a GPIO pin (0-57).
    pub fn set_pull(&self, pin: u32, pull: Pull) {
        let reg = self.base + GPIO_PUP_PDN_CNTRL_REG0 + (pin / 16) as usize * 4;
        let shift = (pin % 16) * 2;
        let mut value = mmio::read(reg);
        value &= !(0b11 << shift);
        value |= (pull as u32) << shift;
        mmio::write(reg, value);
    }
}
