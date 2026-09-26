// AArch64 entry point for the Raspberry Pi 4 and 5.
//
// Rust port of boot.S from https://wiki.osdev.org/Raspberry_Pi_Bare_Bones.
//
// The firmware loads kernel8.img (normally at 0x80000) and jumps to its first
// byte with:
//   x0 = physical address of the device tree blob (DTB)
//   x1, x2, x3 = 0 (reserved)
// The MMU and data cache are off, and we are at EL2 (EL3 under QEMU without
// firmware); _start drops to EL1. Normally only the primary core arrives
// here (the others wait in the firmware's spin loop on the Pi 4, or are
// powered off until a PSCI call on the Pi 5), but we check anyway so a
// different boot stub can't hurt us.

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

    // Drop to EL1, where kernels normally run: it has the usual kernel
    // registers and timer, and leaves EL2 free. The Pi firmware enters at
    // EL2; QEMU without firmware enters at EL3.
    mrs     x2, CurrentEL
    lsr     x2, x2, #2
    cmp     x2, #3
    b.eq    .Lfrom_el3
    cmp     x2, #2
    b.eq    .Lfrom_el2
    b       .Lat_el1

.Lfrom_el3:
    // Non-secure, EL2 is AArch64, HVC enabled, SMC disabled.
    ldr     x2, =0x5b1                  // NS | RES1 (4, 5) | SMD | HCE | RW
    msr     scr_el3, x2
    mov     x2, #0x3c9                  // EL2h, DAIF masked
    msr     spsr_el3, x2
    adr     x2, .Lfrom_el2
    msr     elr_el3, x2
    eret

.Lfrom_el2:
    // EL1 runs AArch64 and nothing is trapped to EL2.
    mov     x2, #(1 << 31)              // HCR_EL2.RW
    msr     hcr_el2, x2
    mov     x2, #0x33ff                 // CPTR_EL2: RES1 bits, FP/SIMD not trapped
    msr     cptr_el2, x2
    msr     hstr_el2, xzr
    // Let EL1 use the physical counter and timer.
    mrs     x2, cnthctl_el2
    orr     x2, x2, #3                  // EL1PCTEN | EL1PCEN
    msr     cnthctl_el2, x2
    msr     cntvoff_el2, xzr
    // At EL1, reads of MIDR/MPIDR return these, so pass the real values on.
    mrs     x2, midr_el1
    msr     vpidr_el2, x2
    mrs     x2, mpidr_el1
    msr     vmpidr_el2, x2
    // EL1 starts with the MMU and caches off.
    ldr     x2, =0x30d00800             // SCTLR_EL1 RES1 bits
    msr     sctlr_el1, x2
    mov     x2, #0x3c5                  // EL1h, DAIF masked
    msr     spsr_el2, x2
    adr     x2, .Lat_el1
    msr     elr_el2, x2
    eret

.Lat_el1:
    mov     sp, x1

    // Don't trap FP/SIMD at EL1 (we build soft-float, but be safe).
    mov     x2, #(3 << 20)              // CPACR_EL1.FPEN
    msr     cpacr_el1, x2

    // Install the exception vectors.
    ldr     x2, =exception_vectors
    msr     vbar_el1, x2
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
    bl      enable_mmu

    // Hand over to Rust: kernel_main(dtb). All Rust code runs with the MMU
    // and caches on.
    mov     x0, x19
    bl      kernel_main

    // kernel_main should never return, but if it does, halt this core.
    b       .Lpark

// Page tables and MMU setup. The memory map is described in mmu.rs; keep
// the two in step.

// Memory attributes (MAIR_EL1 indices):
//   0: Device-nGnRnE           peripherals
//   1: Normal, write-back      RAM
//   2: Normal, non-cacheable   the framebuffer (set up later by mmu.rs)
.equ MAIR_VALUE,        0x44ff00
// TCR_EL1: 39-bit addresses from TTBR0 (T0SZ = 25), 4 KiB granule, table
// walks inner-shareable write-back, TTBR1 walks disabled, 40-bit physical
// addresses.
.equ TCR_VALUE,         0x200993519
// Block descriptors: AF | attribute index | (shareability, execute-never).
.equ NORMAL_BLOCK,      0x705
.equ DEVICE_BLOCK,      0x60000000000401
.equ TABLE,             0x3
.equ SCTLR_M,           (1 << 0)            // MMU
.equ SCTLR_C,           (1 << 2)            // data cache
.equ SCTLR_I,           (1 << 12)           // instruction cache

.section ".bss.page_tables", "aw", @nobits
.balign 4096
.global page_table_l1
page_table_l1:
    .skip 4096                              // 512 x 1 GiB
.global page_table_l2
page_table_l2:
    .skip 4096                              // first GiB as 512 x 2 MiB

.section ".text", "ax"

// Build an identity map and turn on the MMU and caches. Must run at EL1
// with the MMU off, after .bss is zeroed.
enable_mmu:
    ldr     x0, =page_table_l1
    ldr     x1, =page_table_l2

    // L1[0] -> L2: the first GiB, all RAM (including the GPU's share),
    // write-back cacheable.
    orr     x2, x1, #TABLE
    str     x2, [x0]
    mov     x2, #0
    mov     x3, #NORMAL_BLOCK
    mov     x4, #512
1:  orr     x5, x2, x3
    str     x5, [x1], #8
    add     x2, x2, #0x200000
    subs    x4, x4, #1
    b.ne    1b

    // Peripherals, as 1 GiB device blocks. One table serves both boards:
    // mapping the other board's ranges does no harm as nothing touches them.
    ldr     x3, =DEVICE_BLOCK
    ldr     x2, =0xc0000000                 // Pi 4: 0xfc00_0000-0xffff_ffff
    orr     x2, x2, x3
    str     x2, [x0, #3 * 8]
    ldr     x2, =0x1000000000               // Pi 5: PCIe controllers
    orr     x2, x2, x3
    str     x2, [x0, #64 * 8]
    ldr     x2, =0x1040000000               // Pi 5: SoC peripherals, GIC
    orr     x2, x2, x3
    str     x2, [x0, #65 * 8]
    ldr     x2, =0x1f00000000               // Pi 5: RP1, through PCIe
    orr     x2, x2, x3
    str     x2, [x0, #124 * 8]

    // Everything so far was written with the data cache off, straight to
    // memory. Discard any stale cache lines for the kernel's memory (left
    // from before we were loaded) so they can't hide those writes once the
    // cache is on.
    mrs     x3, ctr_el0
    ubfx    x3, x3, #16, #4                 // DminLine: log2(words per line)
    mov     x4, #4
    lsl     x4, x4, x3                      // bytes per line
    sub     x5, x4, #1
    ldr     x1, =__kernel_start
    ldr     x2, =__kernel_end
    bic     x1, x1, x5
2:  dc      ivac, x1
    add     x1, x1, x4
    cmp     x1, x2
    b.lo    2b
    dsb     sy

    ldr     x1, =MAIR_VALUE
    msr     mair_el1, x1
    ldr     x1, =TCR_VALUE
    msr     tcr_el1, x1
    msr     ttbr0_el1, x0
    isb
    tlbi    vmalle1
    ic      iallu
    dsb     nsh
    isb

    mrs     x1, sctlr_el1
    mov     x2, #(SCTLR_M | SCTLR_C | SCTLR_I)
    orr     x1, x1, x2
    msr     sctlr_el1, x1
    isb
    ret

// Exception vector table: 16 entries of 0x80 bytes, 2 KiB aligned.
//
// Every entry saves the interrupted state in a trap frame on the stack
// (x0-x30, ELR, SPSR, ESR, FAR; see `TrapFrame` in exception.rs), calls
// `exception_handler(frame, index)`, then restores the (possibly modified)
// frame and returns. `index` is the entry taken: bits [3:2] say where the
// exception came from and bits [1:0] what kind it was.

.equ FRAME_SIZE, 288

.macro VECTOR index
    .balign 0x80
    sub     sp, sp, #FRAME_SIZE
    stp     x0, x1, [sp, #0]
    mov     x0, #\index
    b       exception_entry
.endm

.section ".text", "ax"
.balign 0x800
.global exception_vectors
exception_vectors:
.irp index, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15
    VECTOR \index
.endr

exception_entry:
    stp     x2, x3, [sp, #16]
    stp     x4, x5, [sp, #32]
    stp     x6, x7, [sp, #48]
    stp     x8, x9, [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x19, [sp, #144]
    stp     x20, x21, [sp, #160]
    stp     x22, x23, [sp, #176]
    stp     x24, x25, [sp, #192]
    stp     x26, x27, [sp, #208]
    stp     x28, x29, [sp, #224]
    mrs     x1, elr_el1
    stp     x30, x1, [sp, #240]
    mrs     x1, spsr_el1
    mrs     x2, esr_el1
    stp     x1, x2, [sp, #256]
    mrs     x1, far_el1
    str     x1, [sp, #272]

    mov     x1, x0                      // index
    mov     x0, sp                      // frame
    bl      exception_handler

    ldp     x30, x1, [sp, #240]
    msr     elr_el1, x1
    ldr     x1, [sp, #256]
    msr     spsr_el1, x1
    ldp     x28, x29, [sp, #224]
    ldp     x26, x27, [sp, #208]
    ldp     x24, x25, [sp, #192]
    ldp     x22, x23, [sp, #176]
    ldp     x20, x21, [sp, #160]
    ldp     x18, x19, [sp, #144]
    ldp     x16, x17, [sp, #128]
    ldp     x14, x15, [sp, #112]
    ldp     x12, x13, [sp, #96]
    ldp     x10, x11, [sp, #80]
    ldp     x8, x9, [sp, #64]
    ldp     x6, x7, [sp, #48]
    ldp     x4, x5, [sp, #32]
    ldp     x2, x3, [sp, #16]
    ldp     x0, x1, [sp, #0]
    add     sp, sp, #FRAME_SIZE
    eret
