#!/usr/bin/env bash
# Assemble the files needed to boot pios on a real Raspberry Pi 4.
#
# Copy the contents of the output directory onto the first (FAT32) partition
# of a micro SD card.
set -euo pipefail

IMG=${1:-kernel8.img}
OUT=${2:-build/sdcard}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
FIRMWARE_URL=${FIRMWARE_URL:-https://raw.githubusercontent.com/raspberrypi/firmware/master/boot}

mkdir -p "$OUT"

# GPU firmware and the board's device tree. These are downloaded once and are
# not part of this repository (they are covered by Broadcom's licence).
for f in start4.elf fixup4.dat bcm2711-rpi-4-b.dtb LICENCE.broadcom; do
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
