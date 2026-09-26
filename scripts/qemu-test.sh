#!/usr/bin/env bash
# Boot kernel8.img on QEMU's raspi4b machine, type a line into UART0 and check
# that the kernel greeted us and echoed the line back.
set -euo pipefail

IMG=${1:-kernel8.img}
QEMU=${QEMU:-qemu-system-aarch64}
DTB=${DTB:-}
TIMEOUT=${TIMEOUT:-10}
INPUT="echo test 123"

if ! "$QEMU" -M help | grep -q '^raspi4b'; then
    echo "error: $QEMU has no raspi4b machine (QEMU >= 9.0 needed)" >&2
    exit 2
fi

out=$(mktemp)
trap 'rm -f "$out"' EXIT

dtb_args=()
[[ -n $DTB ]] && dtb_args=(-dtb "$DTB")

# Feed input after the kernel has had time to boot, then let QEMU run until
# the timeout kills it.
{ sleep 2; printf '%s\r' "$INPUT"; sleep 1; } |
    timeout "$TIMEOUT" "$QEMU" -M raspi4b -kernel "$IMG" "${dtb_args[@]}" \
        -serial stdio -monitor none -display none >"$out" 2>&1 || true

echo "----- UART0 output -----"
tr -d '\r' <"$out"
echo "------------------------"

fail=0
check() {
    if tr -d '\r' <"$out" | grep -qF -- "$1"; then
        echo "PASS: $2"
    else
        echo "FAIL: $2 (expected '$1')"
        fail=1
    fi
}

check "Hello, world!" "kernel prints greeting"
check "for Raspberry Pi 4" "kernel prints banner"
check "Type something" "kernel reaches echo loop"
check "$INPUT" "kernel echoes input"
if grep -qF "KERNEL PANIC" "$out"; then
    echo "FAIL: kernel panicked"
    fail=1
fi

exit $fail
