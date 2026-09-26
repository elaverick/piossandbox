/*
 * Linker script for the Raspberry Pi 4 in AArch64 mode.
 *
 * The GPU firmware loads kernel8.img to 0x80000 and jumps to its first byte,
 * so .text.boot (containing _start) must come first.
 */
ENTRY(_start)

SECTIONS
{
    . = 0x80000;
    __kernel_start = .;

    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    .rodata : ALIGN(8) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(8) {
        *(.data .data.*)
    }

    /* Not stored in the image; zeroed by _start before Rust code runs. */
    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    __kernel_end = .;

    /DISCARD/ : {
        *(.comment) *(.note .note.*) *(.eh_frame .eh_frame_hdr)
    }
}
