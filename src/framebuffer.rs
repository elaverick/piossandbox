//! HDMI output: a framebuffer allocated by the GPU firmware, and a text
//! console drawn on it.
//!
//! The firmware's framebuffer comes from the same mailbox property interface
//! on the Pi 4 and Pi 5, and appears on the first HDMI port (HDMI0, next to
//! the power connector).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::sync::SpinLock;

use pios_textconsole::{MAX_HEIGHT, MAX_WIDTH, Surface, TextConsole};

use crate::addr::PhysAddr;
use crate::mailbox::{self, Mailbox, Message};

/// Used if the firmware can't tell us the display's native resolution.
const DEFAULT_SIZE: (u32, u32) = (1024, 768);

/// A 32 bits-per-pixel framebuffer.
#[derive(Clone, Copy)]
pub struct FrameBuffer {
    /// Physical address of the first pixel.
    base: PhysAddr,
    width: usize,
    height: usize,
    /// Bytes from one row of pixels to the next.
    pitch: usize,
}

impl FrameBuffer {
    /// Ask the firmware for a framebuffer at the display's native resolution.
    /// Returns it and the size of the buffer the firmware allocated. Nothing
    /// may draw on it until the kernel map covers it (see `start`).
    ///
    /// This follows the firmware's documented property interface
    /// (github.com/raspberrypi/firmware/wiki/Mailbox-property-interface) and
    /// the tag sequence Linux's bcm2708_fb driver uses when the firmware
    /// allocates the buffer. The Pi 5's device tree still enables that driver,
    /// so the interface works the same on the Pi 4 and Pi 5.
    pub fn allocate(mailbox: Mailbox) -> Option<(FrameBuffer, usize)> {
        let mut size = [0u32; 2];
        let (width, height) = match mailbox.property(mailbox::TAG_GET_DISPLAY_SIZE, &mut size) {
            Some(())
                if (640..=MAX_WIDTH as u32).contains(&size[0])
                    && (480..=MAX_HEIGHT as u32).contains(&size[1]) =>
            {
                (size[0], size[1])
            }
            _ => DEFAULT_SIZE,
        };

        let mut msg = Message::new();
        let physical = msg.tag(mailbox::TAG_SET_PHYSICAL_SIZE, &[width, height], 2);
        let virt = msg.tag(mailbox::TAG_SET_VIRTUAL_SIZE, &[width, height], 2);
        let offset = msg.tag(mailbox::TAG_SET_VIRTUAL_OFFSET, &[0, 0], 2);
        let depth = msg.tag(mailbox::TAG_SET_DEPTH, &[32], 1);
        let buffer = msg.tag(mailbox::TAG_ALLOCATE_BUFFER, &[4096], 2);
        let pitch = msg.tag(mailbox::TAG_GET_PITCH, &[], 1);
        mailbox.send(&mut msg)?;

        let all_answered = [physical, virt, offset, depth, buffer, pitch]
            .iter()
            .all(|&t| msg.answered(t));
        let fb = FrameBuffer {
            // The firmware returns a VideoCore bus address; the low 30 bits
            // are the ARM physical address.
            base: PhysAddr::new((msg.value(buffer, 0) & 0x3FFF_FFFF) as usize),
            width: msg.value(physical, 0) as usize,
            height: msg.value(physical, 1) as usize,
            pitch: msg.value(pitch, 0) as usize,
        };
        let buffer_size = msg.value(buffer, 1) as usize;
        let valid = all_answered
            && msg.value(depth, 0) == 32
            && fb.base.as_usize() != 0
            && fb.base.as_usize().is_multiple_of(4)
            && (1..=MAX_WIDTH).contains(&fb.width)
            && (1..=MAX_HEIGHT).contains(&fb.height)
            && fb.pitch >= fb.width * 4
            && buffer_size >= fb.pitch * fb.height;
        valid.then_some((fb, buffer_size))
    }

    /// The physical memory holding the pixels. The GPU scans it out of
    /// memory, so the kernel map makes it non-cacheable (writes may still be
    /// combined, which is what makes drawing fast).
    pub fn memory(&self) -> (PhysAddr, usize) {
        (self.base, self.pitch * self.height)
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn base(&self) -> PhysAddr {
        self.base
    }

    /// A surface for drawing on it through the kernel map.
    fn surface(&self) -> Surface {
        // SAFETY: the kernel map covers the framebuffer (see `start`), which
        // is the firmware's and holds no Rust data, and `allocate` checked
        // its size, alignment and pitch.
        unsafe {
            Surface::new(
                self.base.to_virt().as_ptr(),
                self.width,
                self.height,
                self.pitch,
            )
        }
    }

    /// Bytes from one row of pixels to the next.
    pub fn pitch(&self) -> usize {
        self.pitch
    }
}

/// Global state touched only by the primary core, with interrupts off.
///
/// `busy` stops re-entry: if something goes wrong while drawing and the
/// exception handler prints, that output skips the display (it still reaches
/// the serial console) instead of aliasing the state being drawn.
struct Shared<T> {
    busy: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: only one core runs kernel code for now, and `busy` prevents
// re-entrant access from exception handlers.
unsafe impl<T> Sync for Shared<T> {}

impl<T> Shared<T> {
    const fn new(value: T) -> Self {
        Shared {
            busy: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        if self.busy.swap(true, Ordering::Acquire) {
            return None;
        }
        // SAFETY: `busy` guarantees this is the only live reference.
        let result = f(unsafe { &mut *self.value.get() });
        self.busy.store(false, Ordering::Release);
        Some(result)
    }
}

static CONSOLE: Shared<TextConsole> = Shared::new(TextConsole::INACTIVE);

/// What the display console ended up as, for the boot banner.
pub struct DisplayInfo {
    pub fb: FrameBuffer,
    pub columns: usize,
    pub rows: usize,
    /// How many displays the firmware says are attached, if it said.
    pub displays: Option<u32>,
}

/// How many displays the firmware says are attached. Diagnostic only: with
/// hdmi_force_hotplug=1 it drives HDMI even when it counts none.
pub fn count_displays(mailbox: Mailbox) -> Option<u32> {
    let mut count = [0u32; 1];
    mailbox
        .property(mailbox::TAG_GET_NUM_DISPLAYS, &mut count)
        .map(|()| count[0])
}

/// Start the display console on `fb`, which the kernel map must cover.
pub fn start(fb: FrameBuffer, displays: Option<u32>) -> Option<DisplayInfo> {
    CONSOLE.with(|console| {
        console.start(fb.surface());
        *DISPLAY.lock() = Some(fb);
        DisplayInfo {
            fb,
            columns: console.columns(),
            rows: console.rows(),
            displays,
        }
    })
}

/// Write one byte to the display console, if there is one.
pub fn putc(byte: u8) {
    CONSOLE.with(|console| console.putc(byte));
}

/// The framebuffer the kernel's console draws on (or drew on, before
/// handing it over).
static DISPLAY: SpinLock<Option<FrameBuffer>> = SpinLock::new(None);

/// What was on the screen when the kernel handed it over.
pub struct Handover {
    pub fb: FrameBuffer,
    /// The character in each cell, row by row (0 for blank).
    pub text: alloc::vec::Vec<u8>,
    pub columns: usize,
    pub rows: usize,
    pub cursor: (usize, usize),
}

/// Stop drawing on the display, so the console server can take it over,
/// and say what is on it. `None` if there is no display.
pub fn hand_over() -> Option<Handover> {
    let fb = (*DISPLAY.lock())?;
    CONSOLE
        .with(|console| {
            let handover = Handover {
                fb,
                text: console.text().to_vec(),
                columns: console.columns(),
                rows: console.rows(),
                cursor: console.cursor(),
            };
            console.stop();
            handover
        })
        .filter(|handover| !handover.text.is_empty())
}

/// For a kernel panic after the hand-over: take the display back (clearing
/// it), so the report is seen even with only a screen attached.
pub fn take_back() {
    if let Some(fb) = *DISPLAY.lock() {
        CONSOLE.with(|console| {
            if !console.active() {
                console.start(fb.surface());
            }
        });
    }
}
