#!/bin/sh
# Boot a CIBOS bootable disk image in QEMU and capture serial + VGA output.
# Repeatable runtime smoke test for the from-scratch BIOS boot path.
#
# Usage:
#   ./qemu-boot.sh <profile> [seconds] [mem_mib] [key]
#     profile : compute | performance (the bare-buildable profiles)
#     seconds : how long to let it run before capturing/stopping (default 9)
#     mem_mib : guest RAM in MiB (default 128)
#     key     : optional key to inject mid-boot (e.g. 'a') to exercise the
#               keyboard IRQ path; injected ~5s in, during the kernel's
#               keyboard-poll window.
#
# Builds the image first via build-bootimage.sh if it is missing. Prints the
# serial log, then decodes the VGA text buffer (physical 0xB8000, 80x25) so the
# on-screen console can be inspected headlessly.
#
# Requires: qemu-system-x86_64, socat, python3, and the build toolchain.
set -e
. "$HOME/.cargo/env" 2>/dev/null || true

HERE="$(cd "$(dirname "$0")" && pwd)"
PROFILE="${1:?usage: qemu-boot.sh <compute|performance> [seconds] [mem_mib] [key]}"
SECS="${2:-9}"
MEM="${3:-128}"
KEY="${4:-}"

IMG="$HERE/images/cibos-$PROFILE-x86_64.img"
[ -f "$IMG" ] || "$HERE/build-bootimage.sh" "$PROFILE"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "== booting $IMG (${MEM} MiB, ${SECS}s) =="
timeout "$((SECS + 6))" qemu-system-x86_64 \
  -drive "format=raw,file=$IMG" \
  -m "${MEM}" \
  -display none -serial stdio -no-reboot \
  -monitor "unix:$WORK/mon.sock,server,nowait" > "$WORK/serial.txt" 2>"$WORK/err.txt" &
QP=$!
# If a key was requested, inject it repeatedly across a window that overlaps the
# kernel's keyboard wait. That wait happens after boot + the timer self-check
# (several seconds in), so we inject steadily from ~4s through ~10s to be robust
# to boot-timing variance. The keyboard IRQ enqueues each press; the kernel's
# bounded wait catches it.
if [ -n "$KEY" ]; then
  ( sleep 4
    for _ in $(seq 1 24); do
      if [ -S "$WORK/mon.sock" ]; then
        printf 'sendkey %s\n' "$KEY" \
          | timeout 2 socat - "UNIX-CONNECT:$WORK/mon.sock" >/dev/null 2>&1 || true
      fi
      sleep 0.3
    done ) &
fi
sleep "$SECS"
if [ -S "$WORK/mon.sock" ]; then
  printf 'pmemsave 0xB8000 4000 "%s/vga.bin"\n' "$WORK" \
    | timeout 5 socat - "UNIX-CONNECT:$WORK/mon.sock" >/dev/null 2>&1 || true
fi
sleep 1
kill "$QP" 2>/dev/null || true
wait "$QP" 2>/dev/null || true

echo "== SERIAL =="
cat "$WORK/serial.txt"

echo "== VGA (on-screen text console) =="
if [ -f "$WORK/vga.bin" ]; then
  python3 - "$WORK/vga.bin" <<'PY'
import sys
d = open(sys.argv[1], 'rb').read()
for r in range(25):
    row = ''.join(chr(d[(r*80+c)*2]) if 32 <= d[(r*80+c)*2] < 127 else ' ' for c in range(80))
    if row.strip():
        print('|' + row.rstrip())
PY
else
  echo "(no VGA dump captured)"
fi
