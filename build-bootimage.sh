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
# Both x86_64 and i686 produce a complete bootable .img. i686 uses the custom
# target (targets/i686-cibos-none.json) built with nightly + build-std (handled
# automatically below via the I686 flag); the firmware and kernel both build and
# the firmware->kernel handoff is runtime-proven in QEMU (qemu-system-i386). Note
# i686 is serial-only at the kernel (no VGA yet) and its MMU/paging bring-up is
# pending, so i686 boots the kernel but does not yet run the ring-3 app flow.
# aarch64/riscv64 boot via QEMU -kernel/-initrd (build-profile.sh), not a BIOS img.
# Requires: rustup with x86_64-unknown-none, llvm-tools-preview, and GNU binutils
# (gcc/ld/objcopy) for the bootloader.
set -e
. "$HOME/.cargo/env" 2>/dev/null || true

HERE="$(cd "$(dirname "$0")" && pwd)"

PROFILE="${1:?usage: build-bootimage.sh <compute|performance|maximum-isolation|balanced> [arch...]}"
shift

# Optional: --with-apps a,b,c selects which application .capps are baked into the
# image (maps to the kernel's `app-*` features). Defaults to the interactive core
# (login + shell) so a stock image boots straight to the product flow. Pass
# `--with-apps none` for a bare kernel with no apps, or e.g.
# `--with-apps hello,login,shell` to include the demo too.
WITH_APPS="login,shell"
_args=""
while [ $# -gt 0 ]; do
  case "$1" in
    --with-apps) WITH_APPS="$2"; shift 2 ;;
    --with-apps=*) WITH_APPS="${1#--with-apps=}"; shift ;;
    *) _args="$_args $1"; shift ;;
  esac
done
set -- $_args
ARCHES="${*:-x86_64}"

# Translate the app list into kernel feature flags (app-<name>), unless "none".
APP_FEATURES=""
if [ "$WITH_APPS" != "none" ] && [ -n "$WITH_APPS" ]; then
  _IFS_SAVE="$IFS"; IFS=','
  for _app in $WITH_APPS; do
    [ -n "$_app" ] && APP_FEATURES="${APP_FEATURES:+$APP_FEATURES,}app-$_app"
  done
  IFS="$_IFS_SAVE"
fi

case "$PROFILE" in
  compute|performance|maximum-isolation|balanced) ;;
  *) echo "unknown profile: $PROFILE"; exit 1 ;;
esac

# Operational -> firmware feature mapping. All four profiles now produce a
# complete bare image: the portable (no_std) SPHINCS+ verifier lets Standard
# firmware link bare, so maximum-isolation/balanced (signed) work too.
#   SIGNED=1 means the CIBOS image must be signed with the dev key and the
#   firmware embeds the trusted root public key to verify it at boot.
case "$PROFILE" in
  compute)           FW_FEATURES="firmware-bootloader,profile-lightweight";             SIGNED=0 ;;
  performance)       FW_FEATURES="firmware-bootloader,profile-lightweight,smt-enabled"; SIGNED=0 ;;
  balanced)          FW_FEATURES="firmware-bootloader,profile-standard";            SIGNED=1 ;;
  maximum-isolation) FW_FEATURES="firmware-bootloader,profile-standard";            SIGNED=1 ;;
esac

# Standard profiles need a signing keypair. Generate a dev keypair on demand if
# one is not already present (the public half is embedded into the firmware at
# build time via keys/trusted_root.pub).
if [ "$SIGNED" = "1" ]; then
  if [ ! -f "$HERE/keys/trusted_root.pub" ] || [ ! -f "$HERE/keys/dev_signing.key" ]; then
    echo "== generating dev signing keypair (keys/) =="
    mkdir -p "$HERE/keys"
    cargo run -q -p mkimage -- keygen "$HERE/keys/trusted_root.pub" "$HERE/keys/dev_signing.key"
  fi
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
      # 32-bit x86 (legacy BIOS). The kernel needs nightly build-std for the
      # custom target; handled by the I686 flag + the per-arch kernel build below.
      T="$HERE/targets/i686-cibos-none.json"
      KE=0x1000000          # CIBOS kernel load/entry (matches linker/x86_handoff.ld)
      CIBIOS_LOAD=0x100000  # matches cibios/linker/x86.ld (multiboot 1M)
      CIBOS_LOAD=0x4000000
      S2="$HERE/bootloader/build/stage2-i686.bin"
      MKARCH=x86
      I686=1
      ;;
    *) echo "unsupported arch for BIOS image: $a (x86_64 or i686)"; exit 1 ;;
  esac

  echo "== $PROFILE / $a : CIBOS kernel image =="
  # Build the kernel image stamped with the operational profile, objcopy to a
  # flat binary, and wrap into a .cimg via the firmware's own image builder.
  # Standard profiles SIGN the image with the dev key; Lightweight build it
  # unsigned. The profile stamp must match the kernel's compiled profile (the
  # kernel halts on a mismatch).
  if [ "${I686:-0}" = "1" ]; then
    # 32-bit x86 has no precompiled core; use nightly + build-std on the custom
    # target. (Only this arch needs nightly; everything else builds on stable.)
    cargo +nightly build -p kernel-image --no-default-features \
      --features "profile-$PROFILE${EXTRA_KFEATURES:+,$EXTRA_KFEATURES}${APP_FEATURES:+,$APP_FEATURES}" \
      --target "$T" -Z json-target-spec \
      -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem
  else
    cargo build -p kernel-image --no-default-features --features "profile-$PROFILE${EXTRA_KFEATURES:+,$EXTRA_KFEATURES}${APP_FEATURES:+,$APP_FEATURES}" --target "$T"
  fi
  # Cargo's output directory uses the target *name*, which for a custom JSON
  # spec is the file stem (e.g. i686-cibos-none), not the full path in $T.
  case "$T" in
    *.json) TNAME=$(basename "$T" .json) ;;
    *)      TNAME="$T" ;;
  esac

  KBIN="target/$TNAME/debug/cibos-kernel"
  "$LLVM_OC" -O binary "$KBIN" "$HERE/images/kernel-$PROFILE-$a.bin"
  if [ "$SIGNED" = "1" ]; then
    cargo run -q -p mkimage -- sign "$MKARCH" "$KE" "$KE" \
      "$HERE/images/kernel-$PROFILE-$a.bin" "$HERE/keys/dev_signing.key" \
      "$HERE/images/cibos-$PROFILE-$a.cimg" "$PROFILE"
  else
    cargo run -q -p mkimage -- build "$MKARCH" "$KE" "$KE" \
      "$HERE/images/kernel-$PROFILE-$a.bin" "$HERE/images/cibos-$PROFILE-$a.cimg" "$PROFILE"
  fi

  echo "== $PROFILE / $a : CIBIOS firmware (firmware-bootloader) =="
  if [ "${I686:-0}" = "1" ]; then
    cargo +nightly build -p cibios --no-default-features --features "$FW_FEATURES" \
      --target "$T" -Z json-target-spec \
      -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem
  else
    cargo build -p cibios --no-default-features --features "$FW_FEATURES" --target "$T"
  fi
  CIBIOS_ELF="target/$TNAME/debug/cibios"
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
