//! The console server's protocol, and the client side of it.
//!
//! Clients hold a send-only handle to the server's endpoint, with a badge
//! the server knows as a client's. Text travels in the data words of a
//! message: the first word is the byte count, the rest the bytes.
//!
//! | Label | Request data | Reply |
//! | --- | --- | --- |
//! | `WRITE` | byte count, bytes | label `OK` |
//! | `READ` | the most bytes wanted | label `OK`, byte count (at least 1), bytes; waits until there is input |
//!
//! `init` sets the server up before anything else, through a handle with
//! `SETUP_BADGE`, passing it the hardware the kernel handed over:
//!
//! | Label | Handle | Data |
//! | --- | --- | --- |
//! | `SETUP_UART` | the UART's registers | which UART (0 or 1) |
//! | `SETUP_UART_INTERRUPT` | its interrupt | which UART |
//! | `SETUP_DISPLAY` | the framebuffer | width, height, pitch, size |
//! | `SETUP_DISPLAY_TEXT` | the text on it | size, columns, rows, cursor x, cursor y |
//! | `SETUP_DONE` | | |

use core::mem::ManuallyDrop;
use core::sync::atomic::{AtomicUsize, Ordering};

use pios_abi::{Error, MESSAGE_WORDS};

use crate::{Handle, Message};

pub const WRITE: u64 = 1;
pub const READ: u64 = 2;
pub const SETUP_UART: u64 = 100;
pub const SETUP_UART_INTERRUPT: u64 = 101;
pub const SETUP_DISPLAY: u64 = 102;
pub const SETUP_DISPLAY_TEXT: u64 = 103;
pub const SETUP_DONE: u64 = 104;

/// Reply labels.
pub const OK: u64 = 0;
pub const REFUSED: u64 = 1;

/// Badges the server tells its callers apart by.
pub const SETUP_BADGE: u64 = 1;
pub const CLIENT_BADGE: u64 = 2;

/// The most bytes in one message.
pub const MAX_BYTES: usize = (MESSAGE_WORDS - 1) * 8;

/// A message with `label` carrying `bytes` (at most `MAX_BYTES`).
pub fn pack(label: u64, bytes: &[u8]) -> Message {
    assert!(bytes.len() <= MAX_BYTES);
    let mut message = Message::new(label, &[bytes.len() as u64]);
    for (i, &byte) in bytes.iter().enumerate() {
        message.data[1 + i / 8] |= (byte as u64) << (8 * (i % 8));
    }
    message
}

/// The bytes `pack` put in `message`, copied to `out`; returns how many.
pub fn unpack(message: &Message, out: &mut [u8]) -> usize {
    let count = (message.data[0] as usize).min(MAX_BYTES).min(out.len());
    for (i, byte) in out[..count].iter_mut().enumerate() {
        *byte = (message.data[1 + i / 8] >> (8 * (i % 8))) as u8;
    }
    count
}

/// This program's handle to the console server (0 if none).
static CONSOLE: AtomicUsize = AtomicUsize::new(0);

/// Send `print!` output and `read` requests to the console server through
/// `handle`.
pub fn connect(handle: Handle) {
    let old = CONSOLE.swap(handle.into_raw(), Ordering::Relaxed);
    crate::close_raw(old);
}

pub fn is_connected() -> bool {
    CONSOLE.load(Ordering::Relaxed) != 0
}

/// The console handle, borrowed (it stays in `CONSOLE`).
fn console() -> Result<ManuallyDrop<Handle>, Error> {
    match CONSOLE.load(Ordering::Relaxed) {
        0 => Err(Error::BadHandle),
        raw => Ok(ManuallyDrop::new(Handle::from_raw(raw))),
    }
}

/// Write `bytes` to the console.
pub fn write(bytes: &[u8]) -> Result<(), Error> {
    let console = console()?;
    for chunk in bytes.chunks(MAX_BYTES) {
        let reply = console.call(pack(WRITE, chunk))?;
        if reply.label != OK {
            return Err(Error::InvalidArgument);
        }
    }
    Ok(())
}

/// Wait for input, and read up to `buffer.len()` bytes of it. Returns how
/// many (at least 1).
pub fn read(buffer: &mut [u8]) -> Result<usize, Error> {
    let wanted = buffer.len().min(MAX_BYTES) as u64;
    let reply = console()?.call(Message::new(READ, &[wanted]))?;
    if reply.label != OK {
        return Err(Error::InvalidArgument);
    }
    Ok(unpack(&reply, buffer))
}
