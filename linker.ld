/*
 * Linker script for the Raspberry Pi 4 and 5 in AArch64 mode.
 *
 * The firmware loads kernel8.img to 0x80000 and jumps to its first byte,
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

    /* End of what is stored in kernel8.img (used by _start to relocate). */
    . = ALIGN(16);
    __image_end = .;

    /* Not stored in the image; zeroed by _start before Rust code runs. */
    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    /* The boot stack. */
    .stack (NOLOAD) : ALIGN(16) {
        . += 64K;
        __stack_top = .;
    }

    __kernel_end = .;

    /DISCARD/ : {
        *(.comment) *(.note .note.*) *(.eh_frame .eh_frame_hdr)
    }
}
