// AArch64 entry point for the Raspberry Pi 4 and 5.
//
// Rust port of boot.S from https://wiki.osdev.org/Raspberry_Pi_Bare_Bones.
//
// The firmware loads kernel8.img (normally at 0x80000) and jumps to its first
// byte with:
//   x0 = physical address of the device tree blob (DTB)
//   x1, x2, x3 = 0 (reserved)
// The MMU and data cache are off, and we are usually at EL2. Normally only
// the primary core arrives here (the others wait in the firmware's spin loop
// on the Pi 4, or are powered off until a PSCI call on the Pi 5), but we
// check anyway so a different boot stub can't hurt us.

.section ".text.boot", "ax"

.global _start
_start:
    // Only the primary core (affinity 0.0.0) continues. The Pi 4's
    // Cortex-A72 numbers its cores in Aff0; the Pi 5's Cortex-A76 in Aff1.
    mrs     x1, mpidr_el1
    and     x1, x1, #0xffffff
    cbz     x1, .Lprimary
.Lpark:
    wfe
    b       .Lpark

.Lprimary:
    // Keep the DTB pointer in a callee-saved register.
    mov     x19, x0

    // If we were loaded somewhere other than our link address, copy the
    // image into place. The firmware loads at 0x80000 or on a 2 MiB
    // boundary, so the two copies never overlap the code doing the copying.
    adr     x1, _start                  // where we are
    ldr     x2, =_start                 // where we should be
    ldr     x3, =__image_end
    sub     x3, x3, x2                  // image size, a multiple of 16
    cmp     x1, x2
    b.eq    .Lin_place
    b.lo    .Lcopy_down

.Lcopy_up:                              // loaded too high: copy forwards
    ldp     x4, x5, [x1], #16
    stp     x4, x5, [x2], #16
    subs    x3, x3, #16
    b.ne    .Lcopy_up
    b       .Lcopied

.Lcopy_down:                            // loaded too low: copy backwards
    add     x1, x1, x3
    add     x2, x2, x3
1:  ldp     x4, x5, [x1, #-16]!
    stp     x4, x5, [x2, #-16]!
    subs    x3, x3, #16
    b.ne    1b

.Lcopied:
    // Make the copied instructions visible to instruction fetch, then
    // continue in the copy.
    dsb     sy
    ic      iallu
    dsb     sy
    isb
    ldr     x1, =.Lin_place
    br      x1

.Lin_place:
    // Our stack lives just after the kernel image (see linker.ld). It must
    // not go below 0x80000: on the Pi 5 that memory belongs to the secure
    // firmware (TF-A).
    ldr     x1, =__stack_top
    mov     sp, x1

    // Install the exception vectors for whichever EL we are running at, so
    // that faults are reported instead of silently hanging.
    ldr     x1, =exception_vectors
    mrs     x2, CurrentEL
    lsr     x2, x2, #2
    cmp     x2, #3
    b.eq    .Lvbar_el3
    cmp     x2, #2
    b.eq    .Lvbar_el2
    msr     vbar_el1, x1
    b       .Lvbar_done
.Lvbar_el3:
    msr     vbar_el3, x1
    b       .Lvbar_done
.Lvbar_el2:
    msr     vbar_el2, x1
.Lvbar_done:
    isb

    // Zero the .bss section (both ends are 16-byte aligned by linker.ld).
    ldr     x1, =__bss_start
    ldr     x2, =__bss_end
.Lzero_bss:
    cmp     x1, x2
    b.hs    .Lbss_done
    stp     xzr, xzr, [x1], #16
    b       .Lzero_bss

.Lbss_done:
    // Hand over to Rust: kernel_main(dtb).
    mov     x0, x19
    bl      kernel_main

    // kernel_main should never return, but if it does, halt this core.
    b       .Lpark

// Exception vector table: 16 entries of 0x80 bytes, 2 KiB aligned. Nothing
// is handled yet, so every entry reports the exception and stops. x0 tells
// the handler which of the 16 entries was taken.
.section ".text", "ax"
.balign 0x800
.global exception_vectors
exception_vectors:
.irp index, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15
    .balign 0x80
    mov     x0, #\index
    b       fatal_exception
.endr
