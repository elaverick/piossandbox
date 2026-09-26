# pios

A small command-line operating system for the **Raspberry Pi 4**, written in
Rust with a little AArch64 assembly.

It currently implements the [OSDev "Raspberry Pi Bare Bones"][bare-bones]
tutorial, ported from C to Rust: it brings up UART0, prints a greeting plus a
few facts about the machine, and echoes back anything typed on the serial
console.

```
Hello, world!

pios 0.1.0 for Raspberry Pi 4
  exception level : EL2
  device tree     : 0x08000000 (valid)
  kernel image    : 0x00080000 - 0x00081470
  UART0 clock     : 3000000 Hz, 115200 baud
  board revision  : 0xb03115
  ARM memory      : 0x00000000 - 0x3c000000 (960 MiB)

Type something and it will be echoed back.
```

[bare-bones]: https://wiki.osdev.org/Raspberry_Pi_Bare_Bones

## Layout

| File | Purpose |
| --- | --- |
| `src/boot.s` | `_start`: parks cores 1-3, sets up the stack, zeroes `.bss`, calls `kernel_main` |
| `src/main.rs` | `kernel_main`, the panic handler |
| `src/uart.rs` | PL011 UART0 driver and `print!`/`println!` |
| `src/gpio.rs` | Pin function and pull-up/down configuration (BCM2711) |
| `src/mailbox.rs` | VideoCore mailbox property interface |
| `src/mmio.rs` | Peripheral addresses and volatile register access |
| `linker.ld` | Places the kernel at 0x80000 with `_start` first |
| `boot/config.txt` | Firmware configuration for the SD card |

## Building

You need `rustup` (the toolchain, the `aarch64-unknown-none-softfloat` target
and `llvm-tools` are installed automatically from `rust-toolchain.toml`) and
`make`.

```sh
make            # -> kernel8.img
```

The build prints one expected warning about `strict-align` being an unstable
target feature. We use it deliberately: until the MMU is enabled all memory is
treated as Device memory, where unaligned accesses fault, so the compiler must
not generate them.

## Running in QEMU

The `raspi4b` machine needs **QEMU 9.0 or newer** (Ubuntu 24.04 ships 8.2,
which only emulates up to the Pi 3; build QEMU from source or use a newer
distribution). Point the Makefile at a specific binary with
`make run QEMU=/path/to/qemu-system-aarch64`.

```sh
make run        # UART0 on your terminal; quit with Ctrl-C
make test       # boot, type a line, and check the output
```

To boot with the real Pi 4 device tree (so `x0` points at a DTB, as it does on
hardware), run `make sdcard` first and then
`make run DTB=build/sdcard/bcm2711-rpi-4-b.dtb`.

## Running on a real Raspberry Pi 4

1. `make sdcard` builds the kernel and assembles `build/sdcard/` with
   `kernel8.img`, `config.txt` and the GPU firmware (`start4.elf`,
   `fixup4.dat`) and device tree downloaded from
   [raspberrypi/firmware](https://github.com/raspberrypi/firmware).
2. Format a micro SD card with a single FAT32 partition and copy the contents
   of `build/sdcard/` onto it.
3. Connect a 3.3 V USB-serial adapter to the GPIO header:
   adapter **RX to pin 8** (GPIO 14, TXD), adapter **TX to pin 10**
   (GPIO 15, RXD) and **GND to pin 6**. Do not connect the adapter's 5 V/3.3 V
   power pin.
4. Open the serial port at **115200 8N1** (e.g. `picocom -b 115200
   /dev/ttyUSB0` or PuTTY), insert the card and power on the Pi.

## Differences from the C tutorial

- **Pi 4 GPIO pulls.** The tutorial uses the `GPPUD`/`GPPUDCLK0` sequence,
  which does not exist on the BCM2711. We use the Pi 4's
  `GPIO_PUP_PDN_CNTRL_REG0` instead, and we also explicitly select ALT0 for
  GPIO 14/15, because the Pi 4 firmware routes those pins to the mini UART by
  default.
- **Baud rate divisor.** Rather than hard-coding the divisor for a 3 MHz clock,
  we compute it from the clock rate the firmware reports back from the
  mailbox call (falling back to the 48 MHz default).
- **Interrupts.** `IMSC` is set to 0 (all UART interrupts disabled) — in the
  PL011 a 1 bit *enables* an interrupt source.
- **Device tree.** On AArch64 the firmware passes the DTB address in `x0`
  (there are no ATAGs); `_start` forwards it to `kernel_main`.
- **Soft-float target.** We build for `aarch64-unknown-none-softfloat` so no
  FP/SIMD instructions are emitted before we have set up the FPU trap
  controls.
