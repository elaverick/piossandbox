// Stand-in for the Raspberry Pi 5 firmware, used by the raspi5-pios QEMU
// model. It leaves the hardware the way the real firmware does with
// `enable_rp1_uart=1` in config.txt, then enters the kernel at 0x80000 with
// x0 = DTB address.
//
// Unlike the real firmware (which keeps secondary cores off until a PSCI
// call), every core is sent to the kernel, to check that it parks them.
//
// Assemble with: llvm-mc -triple=aarch64 -filetype=obj fake-firmware.s
// The encoded words are pasted into raspi5-pios.c.

    // RP1 UART0: 115200 baud from a 48 MHz clock, 8n1, FIFOs on, enabled.
    ldr     x1, =0x1f00030000
    mov     w2, #26
    str     w2, [x1, #0x24]         // IBRD
    mov     w2, #3
    str     w2, [x1, #0x28]         // FBRD
    mov     w2, #0x70
    str     w2, [x1, #0x2c]         // LCRH
    mov     w2, #0x301
    str     w2, [x1, #0x30]         // CR: UARTEN | TXE | RXE

    // PCIe link to RP1 up (PCIE_MISC_PCIE_STATUS: PHYLINKUP | DL_ACTIVE).
    ldr     x1, =0x1000124068
    mov     w2, #0x30
    str     w2, [x1]

    ldr     x0, =0x08000000         // DTB address
    ldr     x3, =0x80000            // kernel entry
    br      x3
