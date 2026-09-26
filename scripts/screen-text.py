#!/usr/bin/env python3
"""Turn a QEMU screendump (PPM) of the pios display console back into text.

    scripts/screen-text.py screen.ppm
    scripts/screen-text.py screen.ppm --compare serial.txt

Each character cell is matched exactly against the glyphs in textconsole/src/font.rs.
The cursor (an underline) is ignored; unrecognised cells come out as '?'.

With --compare, the serial console transcript is replayed through a simple
terminal emulator with the same size as the display, and the screen must
match it exactly (the display and the UARTs get the same output).
"""
import re
import sys
from pathlib import Path

FONT_RS = Path(__file__).resolve().parent.parent / "textconsole" / "src" / "font.rs"


def load_font():
    src = FONT_RS.read_text()
    width = int(re.search(r"pub const WIDTH: usize = (\d+);", src).group(1))
    height = int(re.search(r"pub const HEIGHT: usize = (\d+);", src).group(1))
    first = int(re.search(r"pub const FIRST: u8 = (0x[0-9a-f]+);", src).group(1), 16)
    table = src[src.index("pub static GLYPHS"):]
    table = table[table.index("= [") :]
    glyphs = {}
    for i, rows in enumerate(re.findall(r"\[([0-9a-fx,\s]+)\]", table)):
        glyphs[tuple(int(b, 16) for b in rows.split(","))] = chr(first + i)
    glyphs[(0,) * height] = " "
    return width, height, glyphs


def read_ppm(path):
    data = Path(path).read_bytes()
    # Header: P6 <width> <height> <maxval>, then RGB bytes.
    fields = re.match(rb"P6\s+(\d+)\s+(\d+)\s+(\d+)\s", data)
    width, height = int(fields.group(1)), int(fields.group(2))
    return width, height, data[fields.end():]


def decode(path):
    """The characters on screen, as a list of lines, and the column count."""
    fw, fh, glyphs = load_font()
    width, height, pixels = read_ppm(path)
    scale = 2 if width >= 1600 else 1  # must match framebuffer.rs
    columns = width // (fw * scale)

    def lit(x, y):
        return pixels[(y * width + x) * 3] > 0x60

    lines = []
    for row in range(height // (fh * scale)):
        line = []
        for column in range(columns):
            cell = []
            for gy in range(fh):
                bits = 0
                for gx in range(fw):
                    x = (column * fw + gx) * scale
                    y = (row * fh + gy) * scale
                    bits |= lit(x, y) << (fw - 1 - gx)
                cell.append(bits)
            ch = glyphs.get(tuple(cell))
            if ch is None:  # maybe the cursor is in this cell
                ch = glyphs.get(tuple(cell[:-2] + [0, 0]), "?")
            line.append(ch)
        lines.append("".join(line).rstrip())
    return lines, columns


def emulate(text, columns, rows):
    """What a display of this size shows after receiving `text`. Mirrors the
    TextConsole in framebuffer.rs."""
    grid = [[" "] * columns for _ in range(rows)]
    x = y = 0

    def newline():
        nonlocal x, y
        x = 0
        if y + 1 < rows:
            y += 1
        else:
            grid.pop(0)
            grid.append([" "] * columns)

    for ch in text:
        if ch == "\n":
            newline()
        elif ch == "\r":
            x = 0
        elif ch == "\b":
            x = max(0, x - 1)
        elif ch == "\t":
            for _ in range(8 - x % 8):
                if x >= columns:
                    newline()
                grid[y][x] = " "
                x += 1
        elif ord(ch) < 0x20 or ch == "\x7f":
            pass
        else:
            if x >= columns:
                newline()
            grid[y][x] = ch if 0x20 <= ord(ch) <= 0x7E else "?"
            x += 1
    return ["".join(line).rstrip() for line in grid]


def main():
    screen, columns = decode(sys.argv[1])
    if len(sys.argv) == 2:
        print("\n".join(screen))
        return

    serial = Path(sys.argv[3]).read_text(errors="replace")
    # Drop QEMU's own messages, which only go to the terminal (and can land
    # in the middle of a line of kernel output).
    serial = re.sub(r"qemu-system-aarch64: [^\n]*\n?", "", serial)
    expected = emulate(serial, columns, len(screen))
    if screen == expected:
        print(f"display matches the serial console ({columns}x{len(screen)})")
        return
    print("display does not match the serial console:")
    for i, (got, want) in enumerate(zip(screen, expected)):
        mark = "  " if got == want else "!="
        print(f"{i:3} {mark} {got!r}")
        if got != want:
            print(f"       want {want!r}")
    sys.exit(1)


if __name__ == "__main__":
    main()
