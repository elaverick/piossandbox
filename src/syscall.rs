//! System calls: the kernel side of the interface in the `pios-abi` crate.

use pios_abi::{Error, call};

use crate::exception::TrapFrame;
use crate::user::UserSlice;

/// Handle a system call from a user program: the call number is in x8,
/// the arguments in x0-x5, and the result goes back in x0.
pub fn handle(frame: &mut TrapFrame) {
    let args = [
        frame.x[0], frame.x[1], frame.x[2], frame.x[3], frame.x[4], frame.x[5],
    ]
    .map(|a| a as usize);
    let result = match frame.x[8] as usize {
        call::DEBUG_WRITE => debug_write(args[0], args[1]),
        call::EXIT => crate::process::exit(args[0] as i32),
        call::YIELD => {
            crate::thread::yield_now();
            Ok(0)
        }
        _ => Err(Error::NoSuchCall),
    };
    frame.x[0] = match result {
        Ok(value) => value as u64,
        Err(error) => error.to_raw() as u64,
    };
}

/// `debug_write(ptr, len)`: write the program's bytes to the kernel
/// console.
fn debug_write(addr: usize, len: usize) -> Result<usize, Error> {
    if len > pios_abi::DEBUG_WRITE_MAX {
        return Err(Error::InvalidArgument);
    }
    let text = UserSlice::new(addr, len)?;
    let mut buffer = [0u8; 256];
    let mut done = 0;
    while done < text.len() {
        let chunk = &mut buffer[..(text.len() - done).min(256)];
        text.read(done, chunk);
        crate::console::write_bytes(chunk);
        done += chunk.len();
    }
    Ok(len)
}
