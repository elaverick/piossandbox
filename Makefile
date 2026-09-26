# Build a Raspberry Pi 4 kernel image (kernel8.img) and run it in QEMU.
#
#   make            build kernel8.img
#   make run        boot it in QEMU (needs QEMU >= 9.0 for the raspi4b machine)
#   make test       boot it in QEMU and check the serial output
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
# Optionally pass a device tree, e.g. DTB=build/sdcard/bcm2711-rpi-4-b.dtb
DTB ?=
QEMU_ARGS := -M raspi4b -kernel $(IMG) -serial stdio -display none \
             $(if $(DTB),-dtb $(DTB),)

.PHONY: all build run test sdcard disasm clippy clean

all: $(IMG)

build:
	cargo build $(CARGO_PROFILE)

$(IMG): build
	$(OBJCOPY) -O binary $(ELF) $(IMG)
	@echo "Built $(IMG) ($$(wc -c < $(IMG)) bytes)"

run: $(IMG)
	$(QEMU) $(QEMU_ARGS)

test: $(IMG)
	QEMU="$(QEMU)" DTB="$(DTB)" ./scripts/qemu-test.sh $(IMG)

sdcard: $(IMG)
	./scripts/make-sdcard.sh $(IMG) build/sdcard

disasm: build
	$(OBJDUMP) -d --no-show-raw-insn $(ELF) | less

clippy:
	cargo clippy $(CARGO_PROFILE)

clean:
	cargo clean
	rm -rf $(IMG) build
