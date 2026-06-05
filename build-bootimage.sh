#!/bin/sh
# Build a complete, flashable CIBOS boot image (.img) for the bare-metal BIOS
# path: from-scratch bootloader + CIBIOS firmware (firmware-bootloader entry) +
# the CIBOS kernel image, assembled by mkbootimage. No GRUB, no multiboot.
#
# This is the x86-family counterpart to build-profile.sh (which targets the QEMU
# multiboot path and all three architectures). A from-scratch BIOS bootloader is
# x86-only by nature, so this script targets x86_64 and i686; aarch64/riscv64
# boot via the platform device tree, not a BIOS .img (use build-profile.sh and
# the FDT/initrd path for those).
#
# Operational <-> firmware pairing (mirrors build-profile.sh and is honest about
# what forms a complete BARE pair today):
#
#   compute, performance        : pair with Lightweight firmware (physical-trust
#       handoff). The firmware builds bare with firmware-bootloader, so a
#       complete flashable .img is produced now.
#   maximum-isolation, balanced : require Standard (signed) firmware. The
#       SPHINCS+ verifier is host-only until the no_std port (SECURITY-NOTES.md),
#       and Standard firmware pulls in pqcrypto (needs libc), so it does NOT link
#       bare yet. These profiles therefore cannot produce a bare .img until the
#       no_std verifier lands; this script says so and stops rather than emit a
#       broken image.
#
# Usage:
#   ./build-bootimage.sh <compute|performance|maximum-isolation|balanced> [arch...]
#     arch defaults to: x86_64
#
# Only x86_64 produces a complete bootable .img today. The i686 path has two
# pre-existing prerequisites that are NOT met yet (both outside the bootloader
# layers, recorded here so they are not lost):
#   1. `mkimage` accepts only the x86_64/aarch64/riscv64 arch tags; it cannot
#      stamp the `x86` (32-bit) tag that ProcessorArchitecture::X86 = 3 needs, so
#      CIBIOS would reject an i686 .cimg as a wrong-arch image at boot.
#   2. `kernel-image`'s 32-bit x86 arch backend is incomplete: it is missing
#      `arch::putc`, `arch::init_serial`, and `arch::halt`, and has two type
#      mismatches, so `cargo build -p kernel-image` does not compile for
#      i686-cibos-none. (CIBIOS itself builds i686 fine on both boot paths; this
#      gap is in the kernel image, not the firmware or the bootloader.)
# When both are addressed, add i686 back to the arch loop below.
#
# Requires: rustup with x86_64-unknown-none, llvm-tools-preview, and GNU binutils
# (gcc/ld/objcopy) for the bootloader.
set -e
. "$HOME/.cargo/env" 2>/dev/null || true

HERE="$(cd "$(dirname "$0")" && pwd)"

PROFILE="${1:?usage: build-bootimage.sh <compute|performance|maximum-isolation|balanced> [arch...]}"
shift
ARCHES="${*:-x86_64}"

case "$PROFILE" in
  compute|performance|maximum-isolation|balanced) ;;
  *) echo "unknown profile: $PROFILE"; exit 1 ;;
esac

# Operational -> firmware feature mapping, and whether a bare image is buildable.
case "$PROFILE" in
  compute)     FW_FEATURES="firmware-bootloader,profile-lightweight";             BARE=1 ;;
  performance) FW_FEATURES="firmware-bootloader,profile-lightweight,smt-enabled"; BARE=1 ;;
  maximum-isolation|balanced) FW_FEATURES="firmware-bootloader,std,profile-standard"; BARE=0 ;;
esac

if [ "$BARE" = "0" ]; then
  echo "ERROR: profile '$PROFILE' requires Standard (signed) firmware, which does"
  echo "       not link bare yet (the no_std SPHINCS+ verifier is pending; see"
  echo "       SECURITY-NOTES.md). A flashable bare .img for this profile is not"
  echo "       available until then. Use 'compute' or 'performance' for a bare"
  echo "       image now, or build-profile.sh for the host-verified Standard path."
  exit 2
fi

LLVM_OC=$(find "$(rustc --print sysroot)" -name llvm-objcopy | head -1)
[ -n "$LLVM_OC" ] || { echo "llvm-objcopy not found; run: rustup component add llvm-tools-preview"; exit 1; }

# Read an ELF entry point as a 0x-prefixed hex address.
elf_entry() {
  if command -v readelf >/dev/null 2>&1; then
    readelf -h "$1" | awk '/Entry point/{print $NF}'
  else
    "$LLVM_OC" --dump-section .text="/dev/null" "$1" >/dev/null 2>&1 || true
    llvm-readobj --file-headers "$1" | awk '/Entry/{print $2}'
  fi
}

echo "== bootloader (stage1 + stage2 for all x86 variants) =="
"$HERE/bootloader/build.sh" "$HERE/bootloader/build"

mkdir -p "$HERE/images"

for a in $ARCHES; do
  case $a in
    x86_64)
      T=x86_64-unknown-none
      KE=0x1000000          # CIBOS kernel load/entry (matches mkimages.sh)
      CIBIOS_LOAD=0x100000  # matches cibios/linker/x86_64.ld
      CIBOS_LOAD=0x4000000
      S2="$HERE/bootloader/build/stage2-x86_64.bin"
      MKARCH=x86_64
      ;;
    i686)
      echo "ERROR: the i686 bootable .img is not buildable yet. Two prerequisites"
      echo "       (both outside the bootloader layers) are unmet:"
      echo "         1. mkimage cannot stamp the 'x86' (32-bit) arch tag, so CIBIOS"
      echo "            would reject the .cimg as wrong-arch at boot."
      echo "         2. kernel-image's 32-bit x86 arch backend is incomplete"
      echo "            (missing arch::putc/init_serial/halt; two type mismatches)."
      echo "       The bootloader, the boot contract, mkbootimage, and CIBIOS i686"
      echo "       all build; only the kernel image and mkimage arch tag are gating."
      exit 3
      ;;
    *) echo "unsupported arch for BIOS image: $a (only x86_64 today)"; exit 1 ;;
  esac

  echo "== $PROFILE / $a : CIBOS kernel image =="
  # Build the kernel image stamped with the operational profile, objcopy to a
  # flat binary, and wrap into a .cimg via the firmware's own image builder.
  cargo build -p kernel-image --no-default-features --features "profile-$PROFILE" --target "$T"
  KBIN="target/$T/debug/cibos-kernel"
  "$LLVM_OC" -O binary "$KBIN" "$HERE/images/kernel-$PROFILE-$a.bin"
  cargo run -q -p mkimage -- build "$MKARCH" "$KE" "$KE" \
    "$HERE/images/kernel-$PROFILE-$a.bin" "$HERE/images/cibos-$PROFILE-$a.cimg" "$PROFILE"

  echo "== $PROFILE / $a : CIBIOS firmware (firmware-bootloader) =="
  cargo build -p cibios --no-default-features --features "$FW_FEATURES" --target "$T"
  CIBIOS_ELF="target/$T/debug/cibios"
  "$LLVM_OC" -O binary "$CIBIOS_ELF" "$HERE/images/cibios-$PROFILE-$a.bin"
  CIBIOS_ENTRY=$(elf_entry "$CIBIOS_ELF")
  [ -n "$CIBIOS_ENTRY" ] || { echo "could not read CIBIOS entry from $CIBIOS_ELF"; exit 1; }
  echo "   CIBIOS entry: $CIBIOS_ENTRY (load $CIBIOS_LOAD)"

  echo "== $PROFILE / $a : assemble bootable .img =="
  IMG="$HERE/images/cibos-$PROFILE-$a.img"
  cargo run -q -p mkbootimage -- \
    --stage1 "$HERE/bootloader/build/stage1.bin" \
    --stage2 "$S2" \
    --cibios "$HERE/images/cibios-$PROFILE-$a.bin" \
    --cibos  "$HERE/images/cibos-$PROFILE-$a.cimg" \
    --cibios-load "$CIBIOS_LOAD" \
    --cibios-entry "$CIBIOS_ENTRY" \
    --cibos-load "$CIBOS_LOAD" \
    --out "$IMG"
  echo "   -> $IMG"
done

echo
echo "Done: profile=$PROFILE. Flashable image(s) under images/cibos-$PROFILE-<arch>.img"
echo "Flash with e.g.:  dd if=images/cibos-$PROFILE-x86_64.img of=/dev/sdX bs=1M conv=fsync"
echo "(Replace /dev/sdX with your target USB device. This overwrites it.)"
