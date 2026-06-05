#!/usr/bin/env bash
# Assemble and link the CIBOS BIOS bootloader into flat binaries.
#
# Stage 1 is architecture-neutral (BIOS always starts in 16-bit real mode).
# Stage 2 is built twice: x86_64 (ends in long mode) and i686 (ends in 32-bit
# protected mode, -DCIBOS_BOOT32).
#
# Toolchain: GNU binutils (gcc as the assembler driver so the C preprocessor
# handles the `#define`s in the .S sources, plus ld and objcopy). These are
# present by default on the dev image. `OBJCOPY=llvm-objcopy` works too.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:-$HERE/build}"
CC="${CC:-gcc}"
LD="${LD:-ld}"
OBJCOPY="${OBJCOPY:-objcopy}"

mkdir -p "$OUT"

# asm <src.S> <obj> [extra-defines...]
asm() {
    "$CC" -m32 -ffreestanding -nostdlib -fno-pic -Wall -Wextra -c "$1" -o "$2" "${@:3}"
}
# link_bin <script> <out.bin> <obj>
link_bin() {
    "$LD" -m elf_i386 -T "$1" -o "${2%.bin}.elf" "$3"
    "$OBJCOPY" -O binary "${2%.bin}.elf" "$2"
}

echo "[bootloader] stage 1 (MBR)"
asm      "$HERE/boot/stage1.S" "$OUT/stage1.o"
link_bin "$HERE/link/stage1.ld" "$OUT/stage1.bin" "$OUT/stage1.o"
s1="$(stat -c%s "$OUT/stage1.bin")"
test "$s1" -eq 512 || { echo "stage1 not 512 bytes: $s1"; exit 1; }

echo "[bootloader] stage 2 (x86_64, long mode)"
asm      "$HERE/boot/stage2.S" "$OUT/stage2_x64.o"
link_bin "$HERE/link/stage2.ld" "$OUT/stage2-x86_64.bin" "$OUT/stage2_x64.o"

echo "[bootloader] stage 2 (i686, protected mode)"
asm      "$HERE/boot/stage2.S" "$OUT/stage2_i686.o" -DCIBOS_BOOT32
link_bin "$HERE/link/stage2.ld" "$OUT/stage2-i686.bin" "$OUT/stage2_i686.o"

# Stage 1 loads Stage 2 into segment 0 at offset 0x8000, so it must stay within
# a single real-mode segment window (<= 32 KiB). Stage 1 also reads the load
# address and sector count from the descriptor; mkbootimage sizes the descriptor
# from the actual byte length, but the 32 KiB ceiling is a hard real-mode limit.
for v in x86_64 i686; do
    sz="$(stat -c%s "$OUT/stage2-$v.bin")"
    test "$sz" -le 32768 || { echo "stage2-$v too large: $sz bytes (max 32768)"; exit 1; }
    echo "[bootloader]   stage2-$v.bin: $sz bytes"
done

echo "[bootloader] done -> $OUT/{stage1.bin,stage2-x86_64.bin,stage2-i686.bin}"
