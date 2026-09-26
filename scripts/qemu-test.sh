#!/usr/bin/env bash
# Boot kernel8.img in QEMU, type a line into a console UART and check that the
# kernel greeted us and echoed the line back.
#
# Environment:
#   QEMU     qemu-system-aarch64 to use
#   MACHINE  raspi4b (default), or raspi5-pios (see tools/qemu-raspi5)
#   CONSOLE  which of the machine's UARTs to talk to (default 0). On
#            raspi5-pios, 0 is the debug UART and 1 is RP1 UART0.
#   DTB      optional device tree to pass to the kernel
set -euo pipefail

IMG=${1:-kernel8.img}
QEMU=${QEMU:-qemu-system-aarch64}
MACHINE=${MACHINE:-raspi4b}
CONSOLE=${CONSOLE:-0}
DTB=${DTB:-}
TIMEOUT=${TIMEOUT:-15}
INPUT="echo test 123"

if ! "$QEMU" -M help | grep -q "^$MACHINE "; then
    echo "error: $QEMU has no $MACHINE machine" >&2
    [[ $MACHINE == raspi4b ]] && echo "(QEMU >= 9.0 is needed)" >&2
    [[ $MACHINE == raspi5-pios ]] && echo "(build one with tools/qemu-raspi5/build-qemu.sh)" >&2
    exit 2
fi

out=$(mktemp)
trap 'rm -f "$out"' EXIT

args=(-M "$MACHINE" -kernel "$IMG" -monitor none -display none)
[[ -n $DTB ]] && args+=(-dtb "$DTB")
for ((i = 0; i < CONSOLE; i++)); do
    args+=(-serial null)
done
args+=(-serial stdio)

# Feed input after the kernel has had time to boot, then let QEMU run until
# the timeout kills it.
# Then paste a burst much bigger than the kernel's input buffer, which must
# arrive intact.
{
    sleep 2
    printf '%s\r' "$INPUT"
    for i in $(seq -w 1 300); do printf 'burst %s abcdefghijklmnopqrstuvwxyz\r' "$i"; done
    sleep 5
} |
    timeout "$TIMEOUT" "$QEMU" "${args[@]}" >"$out" 2>&1 || true

echo "----- $MACHINE, UART $CONSOLE -----"
tr -d '\r' <"$out" | grep -av "^qemu-system-aarch64: warning: bcm2711 dtc:" | grep -av "^burst "
echo "(and $(grep -ac "^burst " "$out") lines of burst input)"
echo "------------------------"

fail=0
pass() { echo "PASS: $1"; }
failed() { echo "FAIL: $1"; fail=1; }
check() {
    if tr -d '\r' <"$out" | grep -aqF -- "$1"; then pass "$2"; else failed "$2 (expected '$1')"; fi
}

check "Hello, world!" "kernel prints greeting"
check "on Raspberry Pi" "kernel prints banner"
check "running at EL1" "kernel drops to EL1"
check "address spaces, user mode, threads and IPC OK" "exception, interrupt, memory, user mode, thread and IPC self-test passes"
check "Hello from user space!" "a user program runs and prints"
check "[hello exited with code 0]" "the user program exits back to the kernel"
check "init: starting the system from a boot image" "the kernel starts init from the boot image"
check "[init exited with code 0]" "init starts hello, waits for it and exits cleanly"
check "Type something" "kernel reaches echo loop"
check "$INPUT" "kernel echoes input"
expected_burst=$(for i in $(seq -w 1 300); do echo "burst $i abcdefghijklmnopqrstuvwxyz"; done)
if [[ $(tr -d '\r' <"$out" | grep -a "^burst ") == "$expected_burst" ]]; then
    pass "a 12 KB burst of input arrives intact"
else
    failed "a 12 KB burst of input arrives intact"
fi
greetings=$(grep -ao "Hello, world!" "$out" | wc -l)
if [[ $greetings -eq 1 ]]; then
    pass "only one core runs the kernel"
else
    failed "only one core runs the kernel (greeted $greetings times)"
fi
if grep -aqE "KERNEL PANIC|UNHANDLED EXCEPTION" "$out"; then
    failed "kernel crashed"
fi

exit $fail
