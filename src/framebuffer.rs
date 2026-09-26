//! HDMI output: a framebuffer allocated by the GPU firmware, and a text
//! console drawn on it.
//!
//! The firmware's framebuffer comes from the same mailbox property interface
//! on the Pi 4 and Pi 5, and appears on the first HDMI port (HDMI0, next to
//! the power connector).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::font;
use crate::mailbox::{self, Mailbox, Message};

/// The largest display we support (4K).
const MAX_WIDTH: usize = 4096;
const MAX_HEIGHT: usize = 2160;

/// Used if the firmware can't tell us the display's native resolution.
const DEFAULT_SIZE: (u32, u32) = (1024, 768);

/// Light grey on black. Red, green and blue are equal, so these look the same
/// whichever byte order (RGB or BGR) the firmware chose.
const FOREGROUND: u32 = 0xFFC0_C0C0;
const BACKGROUND: u32 = 0xFF00_0000;

/// A 32 bits-per-pixel framebuffer.
#[derive(Clone, Copy)]
pub struct FrameBuffer {
    base: usize,
    width: usize,
    height: usize,
    /// Bytes from one row of pixels to the next.
    pitch: usize,
}

impl FrameBuffer {
    const NONE: FrameBuffer = FrameBuffer {
        base: 0,
        width: 0,
        height: 0,
        pitch: 0,
    };

    /// Ask the firmware for a framebuffer at the display's native resolution.
    pub fn allocate(mailbox: Mailbox) -> Option<FrameBuffer> {
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
            base: (msg.value(buffer, 0) & 0x3FFF_FFFF) as usize,
            width: msg.value(physical, 0) as usize,
            height: msg.value(physical, 1) as usize,
            pitch: msg.value(pitch, 0) as usize,
        };
        let buffer_size = msg.value(buffer, 1) as usize;
        let valid = all_answered
            && msg.value(depth, 0) == 32
            && fb.base != 0
            && fb.base.is_multiple_of(4)
            && (1..=MAX_WIDTH).contains(&fb.width)
            && (1..=MAX_HEIGHT).contains(&fb.height)
            && fb.pitch >= fb.width * 4
            && buffer_size >= fb.pitch * fb.height;
        valid.then_some(fb)
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn base(&self) -> usize {
        self.base
    }

    /// Fill `count` pixels starting at (x, y), within one row.
    fn fill_span(&self, x: usize, y: usize, count: usize, color: u32) {
        let row = (self.base + y * self.pitch + x * 4) as *mut u32;
        for i in 0..count {
            // SAFETY: callers keep (x + count, y) within the framebuffer the
            // firmware allocated for us. Framebuffer memory is shared with
            // the GPU, so writes must not be elided.
            unsafe { row.add(i).write_volatile(color) };
        }
    }

    fn clear(&self, color: u32) {
        for y in 0..self.height {
            self.fill_span(0, y, self.width, color);
        }
    }
}

const MAX_COLUMNS: usize = MAX_WIDTH / font::WIDTH;
const MAX_ROWS: usize = MAX_HEIGHT / font::HEIGHT;

/// A text console on a framebuffer.
///
/// A copy of the characters on screen is kept in `grid`, so that scrolling
/// only redraws the cells whose character changes. Until the MMU and caches
/// are on, framebuffer memory is slow to access, and most of a text screen
/// is blank.
struct TextConsole {
    fb: FrameBuffer,
    /// Each font pixel is drawn as a `scale` x `scale` square.
    scale: usize,
    columns: usize,
    rows: usize,
    /// Cursor position. `x` may equal `columns`, meaning the line is full and
    /// the next printable character wraps.
    x: usize,
    y: usize,
    /// The character in each cell; 0 is blank.
    grid: [u8; MAX_COLUMNS * MAX_ROWS],
}

impl TextConsole {
    /// All zeros, so that the static lives in .bss rather than in the image.
    const INACTIVE: TextConsole = TextConsole {
        fb: FrameBuffer::NONE,
        scale: 0,
        columns: 0,
        rows: 0,
        x: 0,
        y: 0,
        grid: [0; MAX_COLUMNS * MAX_ROWS],
    };

    fn start(&mut self, fb: FrameBuffer) {
        // Double the font size on large displays so it stays readable.
        self.scale = if fb.width >= 1600 { 2 } else { 1 };
        self.columns = fb.width / (font::WIDTH * self.scale);
        self.rows = fb.height / (font::HEIGHT * self.scale);
        self.x = 0;
        self.y = 0;
        self.grid[..self.columns * self.rows].fill(0);
        self.fb = fb;
        fb.clear(BACKGROUND);
        self.draw_cursor(true);
    }

    fn active(&self) -> bool {
        self.fb.base != 0
    }

    fn putc(&mut self, byte: u8) {
        self.draw_cursor(false);
        match byte {
            b'\n' => self.newline(),
            b'\r' => self.x = 0,
            0x08 => self.x = self.x.saturating_sub(1),
            b'\t' => {
                for _ in 0..8 - self.x % 8 {
                    self.print(b' ');
                }
            }
            0x00..=0x1F | 0x7F => {} // other control characters: ignore
            _ => self.print(byte),
        }
        self.draw_cursor(true);
    }

    fn print(&mut self, byte: u8) {
        if self.x >= self.columns {
            self.newline();
        }
        self.grid[self.y * self.columns + self.x] = byte;
        self.draw_cell(self.x, self.y, byte, false);
        self.x += 1;
    }

    fn newline(&mut self) {
        self.x = 0;
        if self.y + 1 < self.rows {
            self.y += 1;
        } else {
            self.scroll();
        }
    }

    /// Move everything up a line, redrawing only the cells that change.
    fn scroll(&mut self) {
        let columns = self.columns;
        for row in 0..self.rows {
            for column in 0..columns {
                let new = if row + 1 < self.rows {
                    self.grid[(row + 1) * columns + column]
                } else {
                    0
                };
                let cell = &mut self.grid[row * columns + column];
                if *cell != new {
                    *cell = new;
                    self.draw_cell(column, row, new, false);
                }
            }
        }
    }

    /// Show or hide the cursor, an underline in the cursor's cell.
    fn draw_cursor(&mut self, visible: bool) {
        if self.x < self.columns {
            let byte = self.grid[self.y * self.columns + self.x];
            self.draw_cell(self.x, self.y, byte, visible);
        }
    }

    fn draw_cell(&self, column: usize, row: usize, byte: u8, cursor: bool) {
        const BLANK: [u8; font::HEIGHT] = [0; font::HEIGHT];
        let glyph = match byte {
            0 | b' ' => &BLANK,
            font::FIRST..=font::LAST => &font::GLYPHS[(byte - font::FIRST) as usize],
            _ => &font::GLYPHS[(b'?' - font::FIRST) as usize],
        };
        let scale = self.scale;
        let left = column * font::WIDTH * scale;
        let top = row * font::HEIGHT * scale;
        for (gy, &bits) in glyph.iter().enumerate() {
            let bits = if cursor && gy >= font::HEIGHT - 2 {
                0xFF
            } else {
                bits
            };
            for sy in 0..scale {
                let y = top + gy * scale + sy;
                for gx in 0..font::WIDTH {
                    let color = if bits & (0x80 >> gx) != 0 {
                        FOREGROUND
                    } else {
                        BACKGROUND
                    };
                    self.fb.fill_span(left + gx * scale, y, scale, color);
                }
            }
        }
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
        // Plain loads and stores only: see console.rs on atomics with the MMU
        // off.
        if self.busy.load(Ordering::Acquire) {
            return None;
        }
        self.busy.store(true, Ordering::Release);
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
}

/// Allocate a framebuffer and start the display console on it.
pub fn init(mailbox: Mailbox) -> Option<DisplayInfo> {
    let fb = FrameBuffer::allocate(mailbox)?;
    CONSOLE.with(|console| {
        console.start(fb);
        DisplayInfo {
            fb,
            columns: console.columns,
            rows: console.rows,
        }
    })
}

/// Write one byte to the display console, if there is one.
pub fn putc(byte: u8) {
    CONSOLE.with(|console| {
        if console.active() {
            console.putc(byte);
        }
    });
}
