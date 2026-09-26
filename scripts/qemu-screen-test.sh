#!/usr/bin/env bash
# Boot kernel8.img on QEMU's raspi4b machine, type enough commands into the
# shell to make the display console scroll (with backspaces, tabs and a line
# that wraps), take a screenshot of the emulated HDMI output and check it
# against the serial console transcript.
set -euo pipefail

IMG=${1:-kernel8.img}
QEMU=${QEMU:-qemu-system-aarch64}
MACHINE=${MACHINE:-raspi4b}
TIMEOUT=${TIMEOUT:-15}
HERE=$(cd "$(dirname "$0")" && pwd)

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Send one command to the QEMU monitor.
monitor() {
    python3 - "$tmp/monitor.sock" "$1" <<'PY'
import socket, sys, time
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
time.sleep(0.2)
s.recv(65536)  # banner and prompt
s.sendall(sys.argv[2].encode() + b"\n")
time.sleep(1)
PY
}

{
    sleep 2
    for i in $(seq -w 1 40); do
        printf 'echo line %s of the scrolling test\r' "$i"
    done
    printf 'echo typo\x7f\x7f\x7f\x7fcorrected\r'
    printf 'echo tab\tstops\tline up\r'
    printf 'echo a long line that is wider than the screen so it wraps %.0s' 1 2
    printf '\r'
    sleep 2
    monitor "screendump $tmp/screen.ppm"
    sleep 1
} | timeout "$TIMEOUT" "$QEMU" -M "$MACHINE" -kernel "$IMG" -serial stdio \
        -display none -monitor "unix:$tmp/monitor.sock,server,nowait" \
        >"$tmp/serial.txt" 2>&1 || true

if [[ ! -s $tmp/screen.ppm ]]; then
    echo "FAIL: no screenshot taken"
    tr -d '\r' <"$tmp/serial.txt"
    exit 1
fi

echo "----- $MACHINE display -----"
python3 "$HERE/screen-text.py" "$tmp/screen.ppm"
echo "---------------------------"

# The screen as text, in a file (grep -q in a pipeline could kill the
# writer with SIGPIPE, which pipefail would count as a failure).
python3 "$HERE/screen-text.py" "$tmp/screen.ppm" >"$tmp/screen.txt"

fail=0
if python3 "$HERE/screen-text.py" "$tmp/screen.ppm" --compare "$tmp/serial.txt"; then
    echo "PASS: display matches the serial console"
else
    echo "FAIL: display matches the serial console"
    fail=1
fi
if grep -qx "line 40 of the scrolling test" "$tmp/screen.txt"; then
    echo "PASS: display shows the last line typed"
else
    echo "FAIL: display shows the last line typed"
    fail=1
fi
if grep -qx "pios> echo corrected" "$tmp/screen.txt"; then
    echo "PASS: backspace erases on the display"
else
    echo "FAIL: backspace erases on the display"
    fail=1
fi
exit $fail
