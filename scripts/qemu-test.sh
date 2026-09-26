#!/usr/bin/env bash
# Boot kernel8.img in QEMU, type commands into the shell through a console
# UART, and check the kernel's banner and the commands' output.
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
# Commands to type, one per line.
COMMANDS=("echo test 123" "hello from the shell" "crashtest" "help")

machines=$("$QEMU" -M help)
if ! grep -q "^$MACHINE " <<<"$machines"; then
    echo "error: $QEMU has no $MACHINE machine" >&2
    [[ $MACHINE == raspi4b ]] && echo "(QEMU >= 9.0 is needed)" >&2
    [[ $MACHINE == raspi5-pios ]] && echo "(build one with tools/qemu-raspi5/build-qemu.sh)" >&2
    exit 2
fi

out=$(mktemp)
clean=$(mktemp)
trap 'rm -f "$out" "$clean"' EXIT

args=(-M "$MACHINE" -kernel "$IMG" -monitor none -display none)
[[ -n $DTB ]] && args+=(-dtb "$DTB")
for ((i = 0; i < CONSOLE; i++)); do
    args+=(-serial null)
done
args+=(-serial stdio)

# Type the commands after the kernel has had time to boot, then paste a
# burst of 300 echo commands, much bigger than the console server's input
# buffer, which must all arrive intact. Then let QEMU run until the timeout
# kills it.
{
    sleep 2
    printf '%s\r' "${COMMANDS[@]}"
    for i in $(seq -w 1 300); do printf 'echo burst %s abcdefghijklmnopqrstuvwxyz\r' "$i"; done
    # Leave the shell (after the burst has been read: input a shell has read
    # but not used goes with it), and check init starts another.
    sleep 3
    printf 'exit\r'
    sleep 1
    printf 'echo after exit\r'
    sleep 3
} |
    timeout "$TIMEOUT" "$QEMU" "${args[@]}" >"$out" 2>&1 || true

echo "----- $MACHINE, UART $CONSOLE -----"
tr -d '\r' <"$out" | grep -av "^qemu-system-aarch64: warning: bcm2711 dtc:" | grep -av "^burst "
echo "(and $(grep -ac "^burst " "$out") lines of burst input)"
echo "------------------------"

fail=0
pass() { echo "PASS: $1"; }
failed() { echo "FAIL: $1"; fail=1; }
# The transcript without carriage returns, in a file: grep -q stops reading
# at the first match, so piping into it could kill the writer with SIGPIPE,
# which pipefail would count as a failed check.
tr -d '\r' <"$out" >"$clean"
check() {
    if grep -aqF -- "$1" "$clean"; then pass "$2"; else failed "$2 (expected '$1')"; fi
}
# Like `check`, but for a whole line.
check_line() {
    if grep -aqxF -- "$1" "$clean"; then pass "$2"; else failed "$2 (expected the line '$1')"; fi
}

check "Hello, world!" "kernel prints greeting"
check "on Raspberry Pi" "kernel prints banner"
check "running at EL1" "kernel drops to EL1"
check "address spaces, user mode, threads and IPC OK" "exception, interrupt, memory, user mode, thread and IPC self-test passes"
check "init: starting the system from a boot image" "the kernel starts init from the boot image"
check "init: the console server has" "init hands the hardware to the console server"
check "Welcome to the pios shell" "init starts the shell"
check "pios> echo test 123" "the shell echoes what is typed"
check_line "test 123" "the shell's echo command works"
check "Hello from user space!" "the shell runs a program through the process manager"
check_line "  arguments: from the shell" "the program gets its arguments"
check "[crashtest was stopped by a fault: bad memory access" "the shell reports a program that faults"
check_line "Programs: crashtest fptest hello usertest" "help lists the programs"
check "init: the shell exited; starting a new one" "init starts a new shell when one exits"
check_line "after exit" "the new shell works"
expected_burst=$(for i in $(seq -w 1 300); do echo "burst $i abcdefghijklmnopqrstuvwxyz"; done)
if [[ $(grep -a "^burst " "$clean") == "$expected_burst" ]]; then
    pass "a 12 KB burst of commands arrives intact and runs"
else
    failed "a 12 KB burst of commands arrives intact and runs"
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
