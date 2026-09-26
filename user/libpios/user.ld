/*
 * Layout of a pios user program. The kernel loads each segment at its
 * address with permissions from its flags: code read-execute, constants
 * read-only, data read-write. Segments start on separate pages so each can
 * have its own permissions. The stack is set up by the kernel.
 */
ENTRY(_start)

SECTIONS
{
    . = 0x400000;

    .text : {
        KEEP(*(.text._start))
        *(.text .text.*)
    }

    . = ALIGN(4096);
    .rodata : {
        *(.rodata .rodata.*)
    }

    . = ALIGN(4096);
    .data : {
        *(.data .data.*)
    }
    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    }

    /DISCARD/ : {
        *(.comment) *(.note .note.*) *(.eh_frame .eh_frame_hdr)
    }
}
