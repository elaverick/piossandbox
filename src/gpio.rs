//! Minimal BCM2711 GPIO support: just enough to route pins to peripherals.

use crate::mmio::{self, GPIO_BASE};

/// Function select registers, 10 pins per register, 3 bits per pin.
const GPFSEL0: usize = GPIO_BASE + 0x00;
/// Pull-up/down control, 16 pins per register, 2 bits per pin. These are new
/// on the BCM2711; the GPPUD/GPPUDCLK dance used on earlier Pis (and in the
/// OSDev tutorial) does nothing on a Pi 4.
const GPIO_PUP_PDN_CNTRL_REG0: usize = GPIO_BASE + 0xE4;

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

/// Select the function of a GPIO pin (0-57).
pub fn set_function(pin: u32, function: Function) {
    let reg = GPFSEL0 + (pin / 10) as usize * 4;
    let shift = (pin % 10) * 3;
    let mut value = mmio::read(reg);
    value &= !(0b111 << shift);
    value |= (function as u32) << shift;
    mmio::write(reg, value);
}

/// Configure the pull-up/pull-down resistor of a GPIO pin (0-57).
pub fn set_pull(pin: u32, pull: Pull) {
    let reg = GPIO_PUP_PDN_CNTRL_REG0 + (pin / 16) as usize * 4;
    let shift = (pin % 16) * 2;
    let mut value = mmio::read(reg);
    value &= !(0b11 << shift);
    value |= (pull as u32) << shift;
    mmio::write(reg, value);
}
