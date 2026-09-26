//! Echoes back whatever is typed, through the console server. (The start of
//! the shell, and what the kernel itself used to do.)

#![no_std]
#![no_main]

use libpios::console::{self, MAX_BYTES};
use libpios::println;

libpios::pios_main!(main);

fn main() -> i32 {
    let Some(handle) = libpios::start_handle() else {
        return 1;
    };
    console::connect(handle);
    println!();
    println!("Type something and it will be echoed back.");

    let mut input = [0u8; MAX_BYTES];
    // Each byte echoes as at most three.
    let mut output = [0u8; MAX_BYTES * 3];
    loop {
        let Ok(count) = console::read(&mut input) else {
            return 1;
        };
        let mut len = 0;
        for &byte in &input[..count] {
            let echo: &[u8] = match byte {
                b'\r' | b'\n' => b"\n",
                // Backspace or Delete: step back, blank the character, step
                // back.
                0x08 | 0x7F => b"\x08 \x08",
                _ => core::slice::from_ref(&byte),
            };
            output[len..len + echo.len()].copy_from_slice(echo);
            len += echo.len();
        }
        if console::write(&output[..len]).is_err() {
            return 1;
        }
    }
}
