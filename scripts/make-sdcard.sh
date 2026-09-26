#!/usr/bin/env bash
# Assemble the files needed to boot pios on a real Raspberry Pi 4 or 5. The
# same card works in either.
#
# Copy the contents of the output directory onto the first (FAT32) partition
# of a micro SD card.
set -euo pipefail

IMG=${1:-kernel8.img}
OUT=${2:-build/sdcard}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
FIRMWARE_URL=${FIRMWARE_URL:-https://raw.githubusercontent.com/raspberrypi/firmware/master/boot}

mkdir -p "$OUT"

# Firmware and device trees, downloaded once; they are not part of this
# repository (they are covered by Broadcom's licence).
#  - start4.elf/fixup4.dat: Pi 4 GPU firmware. (The Pi 5 keeps its firmware
#    in EEPROM and ignores them.)
#  - device trees for the Pi 4B and 400, and the Pi 5B (both chip
#    revisions) and 500. The firmware loads the one for its board and passes
#    it to the kernel.
FILES=(
    start4.elf fixup4.dat LICENCE.broadcom
    bcm2711-rpi-4-b.dtb bcm2711-rpi-400.dtb
    bcm2712-rpi-5-b.dtb bcm2712d0-rpi-5-b.dtb bcm2712-rpi-500.dtb
)
for f in "${FILES[@]}"; do
    if [[ ! -s $OUT/$f ]]; then
        echo "Downloading $f"
        curl -fsSL -o "$OUT/$f" "$FIRMWARE_URL/$f"
    fi
done

cp "$ROOT/boot/config.txt" "$OUT/config.txt"
cp "$IMG" "$OUT/kernel8.img"

echo
echo "SD card files are in $OUT:"
ls -l "$OUT"
echo
echo "Copy them to the FAT32 boot partition of a micro SD card."
