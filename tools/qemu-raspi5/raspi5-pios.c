/*
 * raspi5-pios: a minimal Raspberry Pi 5 model for testing pios.
 *
 * QEMU has no Raspberry Pi 5 machine. This one models only what pios uses,
 * at the addresses from the Pi 5 device tree (bcm2712-rpi-5-b.dtb):
 *
 *   - 4x Cortex-A76 starting at EL2, with MPIDR affinities 0x000, 0x100,
 *     0x200, 0x300 like the real BCM2712 (cores are numbered in Aff1)
 *   - 1 GiB of RAM at 0
 *   - a GIC-400 at 0x10_7fff_9000 (distributor) and 0x10_7fff_a000 (CPU
 *     interface), with the CPU timers on their usual PPIs (EL1 physical
 *     timer = ID 30) and the debug UART on SPI 121, as in the device tree
 *   - the debug UART (PL011) at 0x10_7d00_1000              -> -serial #1
 *   - RP1 UART0 (PL011, GPIO 14/15) at 0x1f_0003_0000        -> -serial #2
 *     (its interrupt, an MSI through PCIe on real hardware, is not wired)
 *   - the RP1 PCIe controller's registers at 0x10_0012_0000 (plain RAM)
 *   - the VideoCore mailbox at 0x10_7c01_3880, answered by QEMU's existing
 *     BCM2835 property and framebuffer models (the property interface is
 *     the same on every Pi), so -display or screendump shows the "HDMI"
 *     output
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
#include "hw/misc/bcm2835_mbox.h"
#include "hw/misc/bcm2835_mbox_defs.h"
#include "hw/misc/bcm2835_property.h"
#include "hw/display/bcm2835_fb.h"
#include "hw/nvram/bcm2835_otp.h"
#include "hw/intc/arm_gic.h"
#include "hw/arm/bsa.h"
#include "sysemu/sysemu.h"
#include "cpu.h"

#define GICD_BASE           0x107fff9000ULL
#define GICC_BASE           0x107fffa000ULL
#define GIC_NUM_IRQS        256         /* including the 32 SGIs and PPIs */
#define DEBUG_UART_SPI      121
#define DEBUG_UART_BASE     0x107d001000ULL
#define RP1_UART0_BASE      0x1f00030000ULL
#define RP1_PCIE_BASE       0x1000120000ULL
#define MBOX_BASE           0x107c013880ULL
/* QEMU's mailbox device has its registers at offset 0x80. */
#define MBOX_DEVICE_BASE    (MBOX_BASE - 0x80)
/* Raspberry Pi 5 Model B, 8 GB, revision 1.0. */
#define BOARD_REVISION      0xd04170
#define VCRAM_SIZE          (16 * MiB)
#define KERNEL_ADDR         0x80000
#define DTB_ADDR            0x08000000

/* fake-firmware.s, assembled with llvm-mc. */
static const uint32_t fake_firmware[] = {
    0x58000201, 0x52800342, 0xb9002422, 0x52800062, 0xb9002822, 0x52800e02,
    0xb9002c22, 0x52806022, 0xb9003022, 0x58000121, 0x52800602, 0xb9000022,
    0xd2a10000, 0xd2a00103, 0xd61f0060, 0x00000000, 0x00030000, 0x0000001f,
    0x00124068, 0x00000010,
};

/*
 * The VideoCore mailbox, with the property channel and framebuffer behind it,
 * wired up the way hw/arm/bcm2835_peripherals.c does it.
 */
static void raspi5_pios_videocore_init(MachineState *ms)
{
    MemoryRegion *sysmem = get_system_memory();
    MemoryRegion *mbox_mr = g_new(MemoryRegion, 1);
    DeviceState *mbox = qdev_new(TYPE_BCM2835_MBOX);
    DeviceState *fb = qdev_new(TYPE_BCM2835_FB);
    DeviceState *property = qdev_new(TYPE_BCM2835_PROPERTY);
    DeviceState *otp = qdev_new(TYPE_BCM2835_OTP);

    memory_region_init(mbox_mr, OBJECT(ms), "raspi5-mbox", MBOX_CHAN_COUNT << 4);

    object_property_add_const_link(OBJECT(mbox), "mbox-mr", OBJECT(mbox_mr));
    sysbus_realize_and_unref(SYS_BUS_DEVICE(mbox), &error_fatal);
    sysbus_mmio_map(SYS_BUS_DEVICE(mbox), 0, MBOX_DEVICE_BASE);

    /* The GPU's memory is the top of RAM, as on real Pis. */
    qdev_prop_set_uint32(fb, "vcram-base", ms->ram_size - VCRAM_SIZE);
    qdev_prop_set_uint32(fb, "vcram-size", VCRAM_SIZE);
    object_property_add_const_link(OBJECT(fb), "dma-mr", OBJECT(sysmem));
    sysbus_realize_and_unref(SYS_BUS_DEVICE(fb), &error_fatal);
    memory_region_add_subregion(mbox_mr, MBOX_CHAN_FB << MBOX_AS_CHAN_SHIFT,
                                sysbus_mmio_get_region(SYS_BUS_DEVICE(fb), 0));
    sysbus_connect_irq(SYS_BUS_DEVICE(fb), 0,
                       qdev_get_gpio_in(mbox, MBOX_CHAN_FB));

    sysbus_realize_and_unref(SYS_BUS_DEVICE(otp), &error_fatal);

    qdev_prop_set_uint32(property, "board-rev", BOARD_REVISION);
    object_property_add_const_link(OBJECT(property), "fb", OBJECT(fb));
    object_property_add_const_link(OBJECT(property), "dma-mr", OBJECT(sysmem));
    object_property_add_const_link(OBJECT(property), "otp", OBJECT(otp));
    sysbus_realize_and_unref(SYS_BUS_DEVICE(property), &error_fatal);
    memory_region_add_subregion(mbox_mr, MBOX_CHAN_PROPERTY << MBOX_AS_CHAN_SHIFT,
                        sysbus_mmio_get_region(SYS_BUS_DEVICE(property), 0));
    sysbus_connect_irq(SYS_BUS_DEVICE(property), 0,
                       qdev_get_gpio_in(mbox, MBOX_CHAN_PROPERTY));
}

/* A GICv2 wired up like the BCM2712's GIC-400. Returns the GIC. */
static DeviceState *raspi5_pios_gic_init(MachineState *ms, DeviceState **cpus)
{
    int ncpus = ms->smp.cpus;
    DeviceState *gic = qdev_new(TYPE_ARM_GIC);
    SysBusDevice *busdev = SYS_BUS_DEVICE(gic);
    /* Which PPI each of the CPU's timer outputs drives. */
    static const int timer_irq[] = {
        [GTIMER_PHYS] = ARCH_TIMER_NS_EL1_IRQ,
        [GTIMER_VIRT] = ARCH_TIMER_VIRT_IRQ,
        [GTIMER_HYP]  = ARCH_TIMER_NS_EL2_IRQ,
        [GTIMER_SEC]  = ARCH_TIMER_S_EL1_IRQ,
    };

    qdev_prop_set_uint32(gic, "revision", 2);
    qdev_prop_set_uint32(gic, "num-cpu", ncpus);
    qdev_prop_set_uint32(gic, "num-irq", GIC_NUM_IRQS);
    sysbus_realize_and_unref(busdev, &error_fatal);
    sysbus_mmio_map(busdev, 0, GICD_BASE);
    sysbus_mmio_map(busdev, 1, GICC_BASE);

    for (int n = 0; n < ncpus; n++) {
        /* Per-CPU inputs follow the shared ones. */
        int ppibase = (GIC_NUM_IRQS - GIC_INTERNAL) + n * GIC_INTERNAL;

        for (int t = 0; t < ARRAY_SIZE(timer_irq); t++) {
            qdev_connect_gpio_out(cpus[n], t,
                                  qdev_get_gpio_in(gic, ppibase + timer_irq[t]));
        }
        sysbus_connect_irq(busdev, n, qdev_get_gpio_in(cpus[n], ARM_CPU_IRQ));
        sysbus_connect_irq(busdev, n + ncpus,
                           qdev_get_gpio_in(cpus[n], ARM_CPU_FIQ));
    }
    return gic;
}

static void raspi5_pios_init(MachineState *ms)
{
    MemoryRegion *sysmem = get_system_memory();
    MemoryRegion *pcie = g_new(MemoryRegion, 1);
    DeviceState *cpus[4];
    DeviceState *gic;

    for (int n = 0; n < ms->smp.cpus; n++) {
        Object *cpu = object_new(ms->cpu_type);

        object_property_set_int(cpu, "mp-affinity", n << 8, &error_abort);
        object_property_set_bool(cpu, "has_el3", false, &error_abort);
        object_property_set_bool(cpu, "has_el2", true, &error_abort);
        object_property_set_int(cpu, "cntfrq", 54000000, &error_abort);
        qdev_realize(DEVICE(cpu), NULL, &error_fatal);
        cpus[n] = DEVICE(cpu);
        object_unref(cpu);
    }

    memory_region_add_subregion(sysmem, 0, ms->ram);
    gic = raspi5_pios_gic_init(ms, cpus);

    pl011_create(DEBUG_UART_BASE, qdev_get_gpio_in(gic, DEBUG_UART_SPI),
                 serial_hd(0));
    pl011_create(RP1_UART0_BASE, NULL, serial_hd(1));

    memory_region_init_ram(pcie, NULL, "rp1-pcie-regs", 0x10000, &error_fatal);
    memory_region_add_subregion(sysmem, RP1_PCIE_BASE, pcie);

    raspi5_pios_videocore_init(ms);

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
