//! VideoCore mailbox "property" interface, used to ask the GPU firmware for
//! information and to change settings such as clock rates.

use crate::addr::{PhysAddr, VirtAddr};
use crate::cache;
use crate::mmio;
use crate::timer::Deadline;

const MBOX_READ: usize = 0x00;
const MBOX_STATUS: usize = 0x18;
const MBOX_WRITE: usize = 0x20;

const MBOX_FULL: u32 = 1 << 31;
const MBOX_EMPTY: u32 = 1 << 30;

/// Channel 8: property tags, ARM to VideoCore.
const CHANNEL_PROPERTY: u32 = 8;

const REQUEST: u32 = 0;
const RESPONSE_SUCCESS: u32 = 0x8000_0000;
const TAG_END: u32 = 0;

/// How long to wait for the firmware before giving up.
const TIMEOUT_US: u64 = 100_000;

pub const TAG_GET_BOARD_REVISION: u32 = 0x0001_0002;
pub const TAG_GET_ARM_MEMORY: u32 = 0x0001_0005;
pub const TAG_GET_VC_MEMORY: u32 = 0x0001_0006;
pub const TAG_SET_CLOCK_RATE: u32 = 0x0003_8002;
pub const TAG_ALLOCATE_BUFFER: u32 = 0x0004_0001;
pub const TAG_GET_DISPLAY_SIZE: u32 = 0x0004_0003;
pub const TAG_GET_PITCH: u32 = 0x0004_0008;
pub const TAG_GET_NUM_DISPLAYS: u32 = 0x0004_0013;
pub const TAG_SET_PHYSICAL_SIZE: u32 = 0x0004_8003;
pub const TAG_SET_VIRTUAL_SIZE: u32 = 0x0004_8004;
pub const TAG_SET_DEPTH: u32 = 0x0004_8005;
pub const TAG_SET_VIRTUAL_OFFSET: u32 = 0x0004_8009;

pub const CLOCK_UART: u32 = 2;

/// A property message holding one or more tags, built up with `tag` and sent
/// with `Mailbox::send`.
///
/// The mailbox only carries the upper 28 bits of the buffer's address, so it
/// must be 16-byte aligned. It is 64-byte aligned, and `words` a multiple of
/// 64 bytes, so that the words sit on cache lines of their own: the GPU
/// reads and writes them in memory, behind the CPU's caches, and we clean
/// and invalidate those lines around each call.
#[repr(C, align(64))]
pub struct Message {
    words: [u32; MESSAGE_WORDS],
    len: usize,
}

/// Room for the largest message this kernel sends (framebuffer setup): 192
/// bytes, three 64-byte cache lines.
const MESSAGE_WORDS: usize = 48;
const _: () = assert!((MESSAGE_WORDS * 4).is_multiple_of(64));

/// Identifies a tag within a `Message`, to read its response values.
#[derive(Clone, Copy)]
pub struct TagRef {
    /// Index of the tag's first value word.
    values: usize,
}

impl Message {
    pub const fn new() -> Self {
        // Words 0 and 1 are the header (total size, request code), filled in
        // by `Mailbox::send`.
        Message {
            words: [0; MESSAGE_WORDS],
            len: 2,
        }
    }

    /// Append a tag with the given request values and room for `resp_words`
    /// response values. Panics if the message is full.
    pub fn tag(&mut self, tag: u32, request: &[u32], resp_words: usize) -> TagRef {
        let words = request.len().max(resp_words);
        // Tag header (id, value buffer size, request code) + values, and
        // leave room for the end tag.
        assert!(
            self.len + 3 + words < MESSAGE_WORDS,
            "mailbox message too long"
        );
        self.words[self.len] = tag;
        self.words[self.len + 1] = (words * 4) as u32;
        self.words[self.len + 2] = REQUEST;
        let values = self.len + 3;
        self.words[values..values + request.len()].copy_from_slice(request);
        self.len = values + words;
        TagRef { values }
    }

    /// Did the firmware answer this tag?
    pub fn answered(&self, tag: TagRef) -> bool {
        self.read(tag.values - 1) & (1 << 31) != 0
    }

    /// Response value `index` of `tag`.
    pub fn value(&self, tag: TagRef, index: usize) -> u32 {
        self.read(tag.values + index)
    }

    /// The GPU writes the reply behind the compiler's back, so read it with
    /// volatile loads.
    fn read(&self, index: usize) -> u32 {
        // SAFETY: the reference is valid and `index` is bounds-checked.
        unsafe { core::ptr::read_volatile(&self.words[index]) }
    }
}

/// The mailbox at `base`.
#[derive(Clone, Copy)]
pub struct Mailbox {
    base: usize,
}

impl Mailbox {
    /// The mailbox whose registers are at physical address `base`.
    pub const fn new(base: PhysAddr) -> Self {
        Mailbox {
            base: base.to_virt().as_usize(),
        }
    }

    /// Send a single property tag to the firmware and wait for the reply.
    ///
    /// `values` holds the request values and is overwritten with the response
    /// values. Returns `None` if the firmware rejected the request or did not
    /// answer in time.
    pub fn property<const N: usize>(&self, tag: u32, values: &mut [u32; N]) -> Option<()> {
        let mut msg = Message::new();
        let t = msg.tag(tag, values, N);
        self.send(&mut msg)?;
        if !msg.answered(t) {
            return None;
        }
        for (i, v) in values.iter_mut().enumerate() {
            *v = msg.value(t, i);
        }
        Some(())
    }

    /// Send a message and wait for the reply. Returns `None` if the firmware
    /// did not answer in time or reported an error; check individual tags
    /// with `Message::answered`.
    pub fn send(&self, msg: &mut Message) -> Option<()> {
        msg.words[msg.len] = TAG_END;
        msg.words[0] = ((msg.len + 1) * 4) as u32;
        msg.words[1] = REQUEST;

        let addr = msg.words.as_mut_ptr() as usize;
        let len = core::mem::size_of_val(&msg.words);
        // The GPU needs the physical address.
        let phys = VirtAddr::new(addr).to_phys()?;
        // Make the request visible to the GPU, which reads memory directly...
        cache::clean(addr, len);
        let answered = self.call(CHANNEL_PROPERTY, phys);
        // ...and drop our cached copy so we read the GPU's reply. (The lines
        // hold nothing else, see `Message`.)
        cache::invalidate(addr, len);
        answered?;
        (msg.read(1) == RESPONSE_SUCCESS).then_some(())
    }

    /// Post a message address to `channel` and wait for the firmware to reply.
    fn call(&self, channel: u32, buffer: PhysAddr) -> Option<()> {
        // The buffer must live below 4 GiB for the 32-bit mailbox register;
        // our kernel and its stack sit just above 0x80000, so that holds.
        let buffer = u32::try_from(buffer.as_usize()).ok()?;
        let value = (buffer & !0xF) | (channel & 0xF);
        let deadline = Deadline::after_us(TIMEOUT_US);

        // Make sure the message has reached memory before the GPU looks at it
        // (the caller has already cleaned it from the cache).
        mmio::dsb();

        while mmio::read(self.base + MBOX_STATUS) & MBOX_FULL != 0 {
            if deadline.expired() {
                return None;
            }
            core::hint::spin_loop();
        }
        mmio::write(self.base + MBOX_WRITE, value);

        loop {
            if deadline.expired() {
                return None;
            }
            if mmio::read(self.base + MBOX_STATUS) & MBOX_EMPTY == 0
                && mmio::read(self.base + MBOX_READ) == value
            {
                break;
            }
            core::hint::spin_loop();
        }
        mmio::dsb();
        Some(())
    }
}
