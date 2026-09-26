//! VideoCore mailbox "property" interface, used to ask the GPU firmware for
//! information and to change settings such as clock rates.

use crate::mmio::{self, MBOX_BASE};

const MBOX_READ: usize = MBOX_BASE + 0x00;
const MBOX_STATUS: usize = MBOX_BASE + 0x18;
const MBOX_WRITE: usize = MBOX_BASE + 0x20;

const MBOX_FULL: u32 = 1 << 31;
const MBOX_EMPTY: u32 = 1 << 30;

/// Channel 8: property tags, ARM to VideoCore.
const CHANNEL_PROPERTY: u32 = 8;

const REQUEST: u32 = 0;
const RESPONSE_SUCCESS: u32 = 0x8000_0000;
const TAG_END: u32 = 0;

pub const TAG_GET_BOARD_REVISION: u32 = 0x0001_0002;
pub const TAG_GET_ARM_MEMORY: u32 = 0x0001_0005;
pub const TAG_SET_CLOCK_RATE: u32 = 0x0003_8002;

pub const CLOCK_UART: u32 = 2;

/// A property message buffer. The mailbox only carries the upper 28 bits of
/// the address, so the buffer must be 16-byte aligned.
#[repr(C, align(16))]
struct Message<const N: usize>([u32; N]);

/// Send a single property tag to the firmware and wait for the reply.
///
/// `values` holds the request values and is overwritten with the response
/// values. Returns `None` if the firmware rejected the request.
pub fn property<const N: usize>(tag: u32, values: &mut [u32; N]) -> Option<()> {
    // Header (size, code) + tag header (id, buffer size, req/resp code)
    // + values + end tag. We use a fixed-size buffer big enough for any tag
    // this kernel sends.
    const MAX_VALUES: usize = 8;
    assert!(N <= MAX_VALUES);
    let len = 2 + 3 + N + 1;
    let mut msg = Message([0u32; 2 + 3 + MAX_VALUES + 1]);

    msg.0[0] = (len * 4) as u32;
    msg.0[1] = REQUEST;
    msg.0[2] = tag;
    msg.0[3] = (N * 4) as u32;
    msg.0[4] = 0;
    msg.0[5..5 + N].copy_from_slice(values);
    msg.0[5 + N] = TAG_END;

    let ptr = msg.0.as_mut_ptr();
    call(CHANNEL_PROPERTY, ptr as usize);

    // The GPU wrote the reply behind the compiler's back, so read it back
    // with volatile loads.
    // SAFETY: `ptr` points into `msg`, which is still alive, and every index
    // is within the buffer.
    unsafe {
        let code = ptr.add(1).read_volatile();
        let tag_code = ptr.add(4).read_volatile();
        if code != RESPONSE_SUCCESS || tag_code & (1 << 31) == 0 {
            return None;
        }
        for (i, v) in values.iter_mut().enumerate() {
            *v = ptr.add(5 + i).read_volatile();
        }
    }
    Some(())
}

/// Post a message address to `channel` and block until the firmware replies.
fn call(channel: u32, addr: usize) {
    // The buffer must live below 4 GiB for the 32-bit mailbox register; our
    // kernel and its stack sit near 0x80000, so that always holds.
    let value = (addr as u32 & !0xF) | (channel & 0xF);

    // Make sure the message has actually reached memory before the GPU looks
    // at it. The data cache is off so no cache maintenance is needed yet.
    mmio::dsb();

    while mmio::read(MBOX_STATUS) & MBOX_FULL != 0 {
        core::hint::spin_loop();
    }
    mmio::write(MBOX_WRITE, value);

    loop {
        while mmio::read(MBOX_STATUS) & MBOX_EMPTY != 0 {
            core::hint::spin_loop();
        }
        if mmio::read(MBOX_READ) == value {
            break;
        }
    }
    mmio::dsb();
}
