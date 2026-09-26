#!/usr/bin/env bash
# Boot kernel8.img on QEMU's raspi4b machine, type enough lines to make the
# display console scroll (with backspaces, tabs and a line that wraps), take a screenshot of the
# emulated HDMI output and check it against the serial console transcript.
set -euo pipefail

IMG=${1:-kernel8.img}
QEMU=${QEMU:-qemu-system-aarch64}
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
        printf 'line %s of the scrolling test\r' "$i"
    done
    printf 'typo\x7f\x7f\x7f\x7fcorrected\r'
    printf 'tab\tstops\tline up\r'
    printf 'a long line that is wider than the screen so that it has to wrap %.0s' 1 2 3
    printf '\r'
    sleep 2
    monitor "screendump $tmp/screen.ppm"
    sleep 1
} | timeout "$TIMEOUT" "$QEMU" -M raspi4b -kernel "$IMG" -serial stdio \
        -display none -monitor "unix:$tmp/monitor.sock,server,nowait" \
        >"$tmp/serial.txt" 2>&1 || true

if [[ ! -s $tmp/screen.ppm ]]; then
    echo "FAIL: no screenshot taken"
    tr -d '\r' <"$tmp/serial.txt"
    exit 1
fi

echo "----- raspi4b display -----"
python3 "$HERE/screen-text.py" "$tmp/screen.ppm"
echo "---------------------------"

fail=0
if python3 "$HERE/screen-text.py" "$tmp/screen.ppm" --compare "$tmp/serial.txt"; then
    echo "PASS: display matches the serial console"
else
    echo "FAIL: display matches the serial console"
    fail=1
fi
if python3 "$HERE/screen-text.py" "$tmp/screen.ppm" | grep -qx "line 40 of the scrolling test"; then
    echo "PASS: display shows the last line typed"
else
    echo "FAIL: display shows the last line typed"
    fail=1
fi
if python3 "$HERE/screen-text.py" "$tmp/screen.ppm" | grep -qx "corrected"; then
    echo "PASS: backspace erases on the display"
else
    echo "FAIL: backspace erases on the display"
    fail=1
fi
exit $fail
