//! A text console drawn on a 32 bits-per-pixel framebuffer, shared by the
//! kernel (which draws its boot messages with it) and the user-space console
//! server (which takes over the screen from the kernel, carrying on where it
//! left off).

#![no_std]

pub mod font;

/// The largest display we support (4K).
pub const MAX_WIDTH: usize = 4096;
pub const MAX_HEIGHT: usize = 2160;

const MAX_COLUMNS: usize = MAX_WIDTH / font::WIDTH;
const MAX_ROWS: usize = MAX_HEIGHT / font::HEIGHT;

/// Light grey on black.
///
/// Pixels are 32 bits with red in the low byte, then green, blue and alpha,
/// as the Linux driver for this framebuffer (bcm2708_fb) sets them up. The
/// firmware's alpha mode is not something we choose, and in one of the
/// documented modes 0 is opaque, so the alpha byte is left at 0 as Linux's
/// console does. Red, green and blue are equal for now, so the colours are
/// right even if a display has red and blue swapped.
const FOREGROUND: u32 = rgb(0xC0, 0xC0, 0xC0);
const BACKGROUND: u32 = rgb(0, 0, 0);

const fn rgb(red: u8, green: u8, blue: u8) -> u32 {
    red as u32 | (green as u32) << 8 | (blue as u32) << 16
}

/// Pixels to draw on: a mapped framebuffer.
#[derive(Clone, Copy)]
pub struct Surface {
    base: *mut u8,
    width: usize,
    height: usize,
    /// Bytes from one row of pixels to the next.
    pitch: usize,
}

// SAFETY: a surface is only a description of memory; `TextConsole`'s users
// make sure only one of them draws at a time.
unsafe impl Send for Surface {}

impl Surface {
    const NONE: Surface = Surface {
        base: core::ptr::null_mut(),
        width: 0,
        height: 0,
        pitch: 0,
    };

    /// A `width` x `height` surface of 32-bit pixels at `base`, `pitch`
    /// bytes from one row to the next.
    ///
    /// # Safety
    ///
    /// `base` must be 4-byte aligned and point to `pitch * height` bytes of
    /// writable memory that stays mapped, and that nothing else uses as
    /// Rust data, for as long as the surface (or a console drawing on it)
    /// is used. `pitch` must be at least `width * 4`, and the size within
    /// `MAX_WIDTH` x `MAX_HEIGHT`.
    pub unsafe fn new(base: *mut u8, width: usize, height: usize, pitch: usize) -> Surface {
        assert!(width <= MAX_WIDTH && height <= MAX_HEIGHT && pitch >= width * 4);
        Surface {
            base,
            width,
            height,
            pitch,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Fill `count` pixels starting at (x, y), within one row.
    fn fill_span(&self, x: usize, y: usize, count: usize, color: u32) {
        debug_assert!(x + count <= self.width && y < self.height);
        // SAFETY: `new`'s caller promised the memory; callers keep the span
        // within the surface.
        let row = unsafe { self.base.add(y * self.pitch + x * 4) } as *mut u32;
        for i in 0..count {
            // SAFETY: as above. Framebuffer memory is read by the GPU, so
            // writes must not be elided.
            unsafe { row.add(i).write_volatile(color) };
        }
    }

    fn clear(&self) {
        for y in 0..self.height {
            self.fill_span(0, y, self.width, BACKGROUND);
        }
    }
}

/// A text console on a framebuffer.
///
/// A copy of the characters on screen is kept in `grid`, so that nothing
/// ever reads the framebuffer back (it isn't cached, so reads are slow) and
/// scrolling only redraws the cells whose character changes: most of a text
/// screen is blank. The grid is also what the kernel hands to the console
/// server, so it can carry on with the same screen.
pub struct TextConsole {
    surface: Surface,
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
    /// All zeros, so that a static one lives in .bss rather than in the
    /// image.
    pub const INACTIVE: TextConsole = TextConsole {
        surface: Surface::NONE,
        scale: 0,
        columns: 0,
        rows: 0,
        x: 0,
        y: 0,
        grid: [0; MAX_COLUMNS * MAX_ROWS],
    };

    fn set_surface(&mut self, surface: Surface) {
        // Double the font size on large displays so it stays readable.
        self.scale = if surface.width >= 1600 { 2 } else { 1 };
        self.columns = surface.width / (font::WIDTH * self.scale);
        self.rows = surface.height / (font::HEIGHT * self.scale);
        self.surface = surface;
    }

    /// Start on a cleared `surface`.
    pub fn start(&mut self, surface: Surface) {
        self.set_surface(surface);
        self.x = 0;
        self.y = 0;
        self.grid[..self.columns * self.rows].fill(0);
        surface.clear();
        self.draw_cursor(true);
    }

    /// Carry on with a `surface` another console drew on: `text` is its
    /// `text()` and (`x`, `y`) its `cursor()`. The screen is left as it is.
    /// Returns false (and stays inactive) if they don't fit the surface.
    pub fn resume(&mut self, surface: Surface, text: &[u8], x: usize, y: usize) -> bool {
        self.set_surface(surface);
        let cells = self.columns * self.rows;
        if text.len() != cells || x > self.columns || y >= self.rows {
            self.surface = Surface::NONE;
            return false;
        }
        self.grid[..cells].copy_from_slice(text);
        self.x = x;
        self.y = y;
        true
    }

    pub fn active(&self) -> bool {
        !self.surface.base.is_null()
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The character in each cell, row by row (0 for blank).
    pub fn text(&self) -> &[u8] {
        &self.grid[..self.columns * self.rows]
    }

    /// The cursor's column and row.
    pub fn cursor(&self) -> (usize, usize) {
        (self.x, self.y)
    }

    /// Stop drawing (the screen belongs to someone else now).
    pub fn stop(&mut self) {
        self.surface = Surface::NONE;
    }

    pub fn putc(&mut self, byte: u8) {
        if !self.active() {
            return;
        }
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
                    self.surface.fill_span(left + gx * scale, y, scale, color);
                }
            }
        }
    }
}
