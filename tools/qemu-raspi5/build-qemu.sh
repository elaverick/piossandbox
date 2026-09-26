#!/usr/bin/env bash
# Build QEMU with the raspi5-pios machine added.
#
#   tools/qemu-raspi5/build-qemu.sh <qemu-source-dir> [install-prefix]
#
# Tested with QEMU 9.2. The source tree is modified in place: raspi5-pios.c
# is copied into hw/arm and added to hw/arm/meson.build.
set -euo pipefail

SRC=$(cd "$1" && pwd)
PREFIX=${2:-$SRC/install}
HERE=$(cd "$(dirname "$0")" && pwd)

cp "$HERE/raspi5-pios.c" "$SRC/hw/arm/raspi5-pios.c"
if ! grep -q raspi5-pios.c "$SRC/hw/arm/meson.build"; then
    echo "arm_ss.add(when: ['CONFIG_RASPI', 'TARGET_AARCH64'], if_true: files('raspi5-pios.c'))" \
        >>"$SRC/hw/arm/meson.build"
fi

mkdir -p "$SRC/build"
cd "$SRC/build"
if [[ ! -f build.ninja ]]; then
    ../configure --target-list=aarch64-softmmu --prefix="$PREFIX" \
        --disable-docs --disable-tools --disable-sdl --disable-gtk --disable-vnc
fi
make -j"$(nproc)"
make install
echo "Installed $PREFIX/bin/qemu-system-aarch64"
