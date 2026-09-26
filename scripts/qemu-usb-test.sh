#!/usr/bin/env bash
# Boot kernel8.img on the raspi5-pios machine with a USB keyboard plugged
# into RP1's USB, type commands into the shell on it (through the QEMU
# monitor's sendkey), and check the serial console shows them run.
#
# Environment:
#   QEMU     a qemu-system-aarch64 with the raspi5-pios machine
set -euo pipefail

IMG=${1:-kernel8.img}
QEMU=${QEMU:-qemu-system-aarch64}
TIMEOUT=${TIMEOUT:-20}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Type keys (QEMU key names; "name:ms" holds a key down for that long).
type_keys() {
    python3 - "$tmp/monitor.sock" "$@" <<'PY'
import socket, sys, time
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
time.sleep(0.2)
s.recv(65536)  # banner and prompt
for key in sys.argv[2:]:
    name, _, hold = key.partition(":")
    s.sendall(("sendkey %s %s\n" % (name, hold)).encode())
    time.sleep(0.12 + int(hold or 0) / 1000)
PY
}

word() { for ((i = 0; i < ${#1}; i++)); do printf '%s\n' "${1:i:1}"; done; }

{
    sleep 4
    # "echo Typed on USB!", with a typo fixed with Backspace on the way.
    type_keys $(word echo) spc shift-t $(word yped) spc $(word on) spc \
        shift-u shift-s shift-b x backspace shift-1 ret
    # A held key repeats.
    type_keys $(word echo) spc x:900 ret
    # Ctrl-C abandons a line.
    type_keys $(word echo) spc $(word abandoned) ctrl-c
    type_keys $(word echo) spc $(word done) ret
    sleep 2
} | timeout "$TIMEOUT" "$QEMU" -M raspi5-pios -kernel "$IMG" -display none \
        -serial stdio -serial null -device usb-kbd \
        -monitor "unix:$tmp/monitor.sock,server,nowait" >"$tmp/out.txt" 2>&1 || true

tr -d '\r' <"$tmp/out.txt" >"$tmp/clean.txt"
echo "----- raspi5-pios, USB keyboard -----"
sed -n '/^init:/,$p' "$tmp/clean.txt"
echo "-------------------------------------"

fail=0
pass() { echo "PASS: $1"; }
failed() { echo "FAIL: $1"; fail=1; }
if grep -aqE "^usb[0-9]: port [0-9]+: keyboard$" "$tmp/clean.txt"; then
    pass "the USB driver finds the keyboard"
else
    failed "the USB driver finds the keyboard"
fi
if grep -aqx "Typed on USB!" "$tmp/clean.txt"; then
    pass "typing on the USB keyboard runs a command (with Shift and Backspace)"
else
    failed "typing on the USB keyboard runs a command (with Shift and Backspace)"
fi
if grep -aqxE "x{8,}" "$tmp/clean.txt"; then
    pass "a held key repeats"
else
    failed "a held key repeats"
fi
if grep -aqx "pios> echo abandoned^C" "$tmp/clean.txt" && grep -aqx "done" "$tmp/clean.txt" \
    && ! grep -aqx "abandoned" "$tmp/clean.txt"; then
    pass "Ctrl-C abandons a line"
else
    failed "Ctrl-C abandons a line"
fi
if grep -aqE "KERNEL PANIC|UNHANDLED EXCEPTION" "$tmp/clean.txt"; then
    failed "kernel crashed"
fi
exit $fail
