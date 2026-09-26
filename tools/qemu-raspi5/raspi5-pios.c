/*
 * raspi5-pios: a minimal Raspberry Pi 5 model for testing pios.
 *
 * QEMU has no Raspberry Pi 5 machine. This one models only what pios uses,
 * at the addresses from the Pi 5 device tree (bcm2712-rpi-5-b.dtb):
 *
 *   - 4x Cortex-A76 starting at EL2, with MPIDR affinities 0x000, 0x100,
 *     0x200, 0x300 like the real BCM2712 (cores are numbered in Aff1)
 *   - 1 GiB of RAM at 0
 *   - the debug UART (PL011) at 0x10_7d00_1000              -> -serial #1
 *   - RP1 UART0 (PL011, GPIO 14/15) at 0x1f_0003_0000        -> -serial #2
 *   - the RP1 PCIe controller's registers at 0x10_0012_0000 (plain RAM)
 *   - the VideoCore mailbox at 0x10_7c01_3880 (unimplemented: never
 *     answers, so pios's mailbox timeouts get exercised)
 *
 * A small fake firmware (fake-firmware.s) at address 0 sets up RP1 UART0
 * and the PCIe link status the way the real firmware does with
 * enable_rp1_uart=1, then jumps to the kernel at 0x80000 on every core.
 *
 * -kernel loads a raw image at 0x80000; -dtb loads a device tree at
 * 0x0800_0000.
 *
 * This is not an emulation of the BCM2712. It checks addresses, board
 * detection and boot flow, not the behaviour of the real hardware.
 *
 * SPDX-License-Identifier: GPL-2.0-or-later
 */

#include "qemu/osdep.h"
#include "qemu/units.h"
#include "qemu/error-report.h"
#include "qapi/error.h"
#include "exec/address-spaces.h"
#include "hw/boards.h"
#include "hw/loader.h"
#include "hw/char/pl011.h"
#include "hw/misc/unimp.h"
#include "sysemu/sysemu.h"
#include "cpu.h"

#define DEBUG_UART_BASE     0x107d001000ULL
#define RP1_UART0_BASE      0x1f00030000ULL
#define RP1_PCIE_BASE       0x1000120000ULL
#define MBOX_BASE           0x107c013880ULL
#define KERNEL_ADDR         0x80000
#define DTB_ADDR            0x08000000

/* fake-firmware.s, assembled with llvm-mc. */
static const uint32_t fake_firmware[] = {
    0x58000201, 0x52800342, 0xb9002422, 0x52800062, 0xb9002822, 0x52800e02,
    0xb9002c22, 0x52806022, 0xb9003022, 0x58000121, 0x52800602, 0xb9000022,
    0xd2a10000, 0xd2a00103, 0xd61f0060, 0x00000000, 0x00030000, 0x0000001f,
    0x00124068, 0x00000010,
};

static void raspi5_pios_init(MachineState *ms)
{
    MemoryRegion *sysmem = get_system_memory();
    MemoryRegion *pcie = g_new(MemoryRegion, 1);

    for (int n = 0; n < ms->smp.cpus; n++) {
        Object *cpu = object_new(ms->cpu_type);

        object_property_set_int(cpu, "mp-affinity", n << 8, &error_abort);
        object_property_set_bool(cpu, "has_el3", false, &error_abort);
        object_property_set_bool(cpu, "has_el2", true, &error_abort);
        object_property_set_int(cpu, "cntfrq", 54000000, &error_abort);
        qdev_realize(DEVICE(cpu), NULL, &error_fatal);
        object_unref(cpu);
    }

    memory_region_add_subregion(sysmem, 0, ms->ram);

    pl011_create(DEBUG_UART_BASE, NULL, serial_hd(0));
    pl011_create(RP1_UART0_BASE, NULL, serial_hd(1));

    memory_region_init_ram(pcie, NULL, "rp1-pcie-regs", 0x10000, &error_fatal);
    memory_region_add_subregion(sysmem, RP1_PCIE_BASE, pcie);

    create_unimplemented_device("mailbox", MBOX_BASE, 0x40);

    rom_add_blob_fixed("fake-firmware", fake_firmware, sizeof(fake_firmware), 0);

    if (ms->kernel_filename &&
        load_image_targphys(ms->kernel_filename, KERNEL_ADDR,
                            ms->ram_size - KERNEL_ADDR) < 0) {
        error_report("could not load kernel '%s'", ms->kernel_filename);
        exit(1);
    }
    if (ms->dtb &&
        load_image_targphys(ms->dtb, DTB_ADDR, ms->ram_size - DTB_ADDR) < 0) {
        error_report("could not load device tree '%s'", ms->dtb);
        exit(1);
    }
}

static void raspi5_pios_machine_init(MachineClass *mc)
{
    mc->desc = "Minimal Raspberry Pi 5 model for pios testing";
    mc->init = raspi5_pios_init;
    mc->default_cpu_type = ARM_CPU_TYPE_NAME("cortex-a76");
    mc->min_cpus = 1;
    mc->max_cpus = 4;
    mc->default_cpus = 4;
    mc->default_ram_size = 1 * GiB;
    mc->default_ram_id = "ram";
    mc->no_parallel = 1;
    mc->no_floppy = 1;
    mc->no_cdrom = 1;
}

DEFINE_MACHINE("raspi5-pios", raspi5_pios_machine_init)
