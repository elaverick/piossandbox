/*
 * Linker script for the Raspberry Pi 4 and 5 in AArch64 mode.
 *
 * The firmware loads kernel8.img to physical 0x80000 and jumps to its first
 * byte, so .text.boot (containing _start) must come first. The kernel is
 * linked where the kernel's linear map puts that address (KERNEL_BASE +
 * 0x80000; see mmu.rs and boot.s).
 *
 * Code, read-only data and writable data start on separate pages, so each
 * can be mapped with its own permissions.
 */
ENTRY(_start)

SECTIONS
{
    . = 0xFFFFFF8000080000;
    __kernel_start = .;

    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }
    . = ALIGN(4096);
    __text_end = .;

    .rodata : {
        *(.rodata .rodata.*)
    }
    . = ALIGN(4096);
    __rodata_end = .;

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

    /* The boot stack, with a guard page below it that mmu.rs leaves
     * unmapped, so overflowing the stack faults instead of overwriting .bss.
     */
    . = ALIGN(4096);
    __stack_guard = .;
    .stack (NOLOAD) : ALIGN(4096) {
        . += 4096;
        __stack_bottom = .;
        . += 64K;
        __stack_top = .;
    }

    . = ALIGN(4096);
    __kernel_end = .;

    /DISCARD/ : {
        *(.comment) *(.note .note.*) *(.eh_frame .eh_frame_hdr)
    }
}
