# Build a Raspberry Pi 4 kernel image (kernel8.img) and run it in QEMU.
#
#   make            build kernel8.img
#   make run        boot it in QEMU's Pi 4 model (needs QEMU >= 9.0)
#   make test       boot it in QEMU's Pi 4 model and check the serial and
#                   HDMI output
#   make run-pi5    boot it in the raspi5-pios model (see tools/qemu-raspi5)
#   make test-pi5   test it on the raspi5-pios model: both UARTs and HDMI
#   make sdcard     assemble a bootable SD card directory in build/sdcard
#   make disasm     disassemble the kernel

TARGET  := aarch64-unknown-none-softfloat
PROFILE ?= release
ELF     := target/$(TARGET)/$(PROFILE)/pios
IMG     := kernel8.img

CARGO_PROFILE := $(if $(filter release,$(PROFILE)),--release,)

# llvm-objcopy/objdump ship with the llvm-tools rustup component (see
# rust-toolchain.toml), so no cross binutils are needed.
LLVM_BIN := $(shell rustc --print sysroot)/lib/rustlib/$(shell rustc -vV | sed -n 's/^host: //p')/bin
OBJCOPY  := $(LLVM_BIN)/llvm-objcopy
OBJDUMP  := $(LLVM_BIN)/llvm-objdump

QEMU ?= qemu-system-aarch64
# A QEMU built with tools/qemu-raspi5/build-qemu.sh, for the Pi 5 targets.
QEMU_PI5 ?= $(QEMU)
# Optionally pass a device tree, e.g. DTB=build/sdcard/bcm2711-rpi-4-b.dtb
DTB ?=
QEMU_ARGS := -kernel $(IMG) $(if $(DTB),-dtb $(DTB),)

.PHONY: all build run run-pi5 test test-pi5 sdcard disasm clippy clean

all: $(IMG)

build:
	cargo build $(CARGO_PROFILE)

$(IMG): build
	$(OBJCOPY) -O binary $(ELF) $(IMG)
	@echo "Built $(IMG) ($$(wc -c < $(IMG)) bytes)"

# Add QEMU_DISPLAY=gtk (or sdl) to see the HDMI output in a window, if your QEMU
# was built with one.
QEMU_DISPLAY ?= none
run: $(IMG)
	$(QEMU) -M raspi4b $(QEMU_ARGS) -serial stdio -display $(QEMU_DISPLAY)

# The debug UART is on your terminal; RP1 UART0 (GPIO 14/15) goes to a file.
run-pi5: $(IMG)
	$(QEMU_PI5) -M raspi5-pios $(QEMU_ARGS) -display $(QEMU_DISPLAY) -serial stdio -serial file:rp1-uart0.log

test: $(IMG)
	QEMU="$(QEMU)" DTB="$(DTB)" ./scripts/qemu-test.sh $(IMG)
	QEMU="$(QEMU)" ./scripts/qemu-screen-test.sh $(IMG)

test-pi5: $(IMG)
	QEMU="$(QEMU_PI5)" MACHINE=raspi5-pios CONSOLE=0 DTB="$(DTB)" ./scripts/qemu-test.sh $(IMG)
	QEMU="$(QEMU_PI5)" MACHINE=raspi5-pios CONSOLE=1 DTB="$(DTB)" ./scripts/qemu-test.sh $(IMG)
	QEMU="$(QEMU_PI5)" MACHINE=raspi5-pios ./scripts/qemu-screen-test.sh $(IMG)

sdcard: $(IMG)
	./scripts/make-sdcard.sh $(IMG) build/sdcard

disasm: build
	$(OBJDUMP) -d --no-show-raw-insn $(ELF) | less

clippy:
	cargo clippy $(CARGO_PROFILE)

clean:
	cargo clean
	rm -rf $(IMG) build
