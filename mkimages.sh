#!/bin/sh
# Build the CIBOS kernel images (.cimg) for all three architectures and verify
# each through the firmware's own parser/verifier. Also builds the x86_64 CIBIOS
# firmware ELF, the other half of the boot pair.
#
# Requires: rustup with the bare targets and llvm-tools-preview
# (rustup component add llvm-tools-preview).
set -e
. "$HOME/.cargo/env" 2>/dev/null || true

LLVM_OC=$(find "$(rustc --print sysroot)" -name llvm-objcopy | head -1)
[ -n "$LLVM_OC" ] || { echo "llvm-objcopy not found; run: rustup component add llvm-tools-preview"; exit 1; }

mkdir -p images
for a in x86_64 aarch64 riscv64; do
  case $a in
    x86_64)  t=x86_64-unknown-none;          e=0x1000000;;
    aarch64) t=aarch64-unknown-none;         e=0x41000000;;
    riscv64) t=riscv64gc-unknown-none-elf;   e=0x81000000;;
  esac
  echo "== $a =="
  cargo build -p kernel-image --no-default-features --target "$t"
  "$LLVM_OC" -O binary "target/$t/debug/cibos-kernel" "images/kernel-$a.bin"
  cargo run -q -p mkimage -- build "$a" "$e" "$e" "images/kernel-$a.bin" "images/cibos-$a.cimg"
  cargo run -q -p mkimage -- verify "images/cibos-$a.cimg" "$a"
done

echo "== CIBIOS firmware (all targets) =="
cargo build -p cibios --target x86_64-unknown-none
cargo build -p cibios --target aarch64-unknown-none
cargo build -p cibios --target riscv64gc-unknown-none-elf
echo "firmware ELFs under target/<triple>/debug/cibios"
echo "Done. Boot a pair per BOOT.md."
