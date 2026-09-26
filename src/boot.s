// AArch64 entry point for the Raspberry Pi 4.
//
// Rust port of boot.S from https://wiki.osdev.org/Raspberry_Pi_Bare_Bones.
//
// The firmware loads kernel8.img at 0x80000 and jumps here with:
//   x0 = physical address of the device tree blob (DTB)
//   x1, x2, x3 = 0 (reserved)
// Normally only core 0 arrives here (cores 1-3 sit in the firmware's spin
// loop), but we check anyway so a different stub can't hurt us.

.section ".text.boot", "ax"

.global _start
_start:
    // Only core 0 continues; any other core parks forever.
    mrs     x1, mpidr_el1
    and     x1, x1, #0xff
    cbz     x1, 2f
1:  wfe
    b       1b

2:  // Keep the DTB pointer in a callee-saved register.
    mov     x19, x0

    // The stack grows down from just below our load address.
    ldr     x1, =_start
    mov     sp, x1

    // Zero the .bss section (both ends are 16-byte aligned by linker.ld).
    ldr     x1, =__bss_start
    ldr     x2, =__bss_end
3:  cmp     x1, x2
    b.hs    4f
    stp     xzr, xzr, [x1], #16
    b       3b

4:  // Hand over to Rust: kernel_main(dtb).
    mov     x0, x19
    bl      kernel_main

    // kernel_main should never return, but if it does, halt this core.
    b       1b
