#!/usr/bin/env bash
# Build the aarch64 CIBOS kernel as a raw ARM64 boot Image (not ELF).
#
# The kernel begins with the standard ARM64 Linux image header, so a conforming
# loader (real U-Boot/UEFI, and QEMU `virt`) passes the DTB pointer in x0 — the
# same on hardware and in QEMU. This is why the kernel needs no DTB fallback.
#
# Usage: ./build-arm64-image.sh   ->   target/aarch64-unknown-none/debug/Image
set -euo pipefail
export RUSTUP_HOME=${RUSTUP_HOME:-/root/.rustup} CARGO_HOME=${CARGO_HOME:-/root/.cargo}
export PATH=$CARGO_HOME/bin:$PATH

cargo build -p kernel-image --target aarch64-unknown-none --features self-boot
ELF=target/aarch64-unknown-none/debug/cibos-kernel
IMG=target/aarch64-unknown-none/debug/Image
OBJCOPY=$(find "$(rustc --print sysroot)" -name llvm-objcopy | head -1)
[ -n "$OBJCOPY" ] || { echo "llvm-objcopy not found; rustup component add llvm-tools-preview"; exit 1; }
"$OBJCOPY" -O binary "$ELF" "$IMG"
echo "Built ARM64 Image: $IMG ($(stat -c%s "$IMG") bytes)"
echo "Boot: qemu-system-aarch64 -machine virt -cpu cortex-a72 -display none -serial stdio -kernel $IMG"
