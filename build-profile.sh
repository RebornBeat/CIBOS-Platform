#!/bin/sh
# Build a complete CIBOS operational profile: the kernel image (stamped with the
# profile) for each architecture, paired with the matching CIBIOS firmware.
#
# This encodes the documented operational<->firmware pairing and is honest about
# which profiles can form a complete BARE boot pair today:
#
#   maximum-isolation, balanced : require Standard (signed) firmware. The
#       SPHINCS+ verifier is host-only until the no_std port (SECURITY-NOTES.md),
#       and Lightweight firmware does not accept these profiles, so there is no
#       complete BARE pair yet. The kernel image is built and the Standard
#       firmware is host-verified; bare signed firmware lands with the verifier.
#   performance : pairs with Lightweight firmware (which accepts Performance) ->
#       complete bare pair now (physical-trust, SMT on). Signed Standard is
#       host-only.
#   compute     : Lightweight firmware -> complete bare pair now (SMT on).
#
# Usage: ./build-profile.sh <maximum-isolation|balanced|performance|compute> [arch...]
#   arch defaults to: x86_64 aarch64 riscv64
#
# Requires: rustup with the bare targets and llvm-tools-preview.
set -e
. "$HOME/.cargo/env" 2>/dev/null || true

PROFILE="${1:?usage: build-profile.sh <maximum-isolation|balanced|performance|compute> [arch...]}"
shift
ARCHES="${*:-x86_64 aarch64 riscv64}"

case "$PROFILE" in
  maximum-isolation|balanced|performance|compute) ;;
  *) echo "unknown profile: $PROFILE"; exit 1 ;;
esac

# Operational -> firmware pairing. FW_BARE=1 means a complete bare boot pair is
# buildable today; FW_BARE=0 means the firmware is host-verified only (T3-C).
case "$PROFILE" in
  compute)                    FW_FEATURES="profile-lightweight";             FW_BARE=1 ;;
  performance)                FW_FEATURES="profile-lightweight,smt-enabled"; FW_BARE=1 ;;
  maximum-isolation|balanced) FW_FEATURES="std,profile-standard";            FW_BARE=0 ;;
esac

LLVM_OC=$(find "$(rustc --print sysroot)" -name llvm-objcopy | head -1)
[ -n "$LLVM_OC" ] || { echo "llvm-objcopy not found; run: rustup component add llvm-tools-preview"; exit 1; }
mkdir -p images

for a in $ARCHES; do
  case $a in
    x86_64)  t=x86_64-unknown-none;        e=0x1000000 ;;
    aarch64) t=aarch64-unknown-none;       e=0x41000000 ;;
    riscv64) t=riscv64gc-unknown-none-elf; e=0x81000000 ;;
    *) echo "unknown arch: $a"; exit 1 ;;
  esac

  echo "== $PROFILE / $a : kernel image =="
  # build.rs validates the feature combination; the profile is stamped into the
  # .cimg so the firmware-derived handoff matches the kernel's compiled profile.
  cargo build -p kernel-image --no-default-features --features "profile-$PROFILE" --target "$t"
  "$LLVM_OC" -O binary "target/$t/debug/cibos-kernel" "images/kernel-$PROFILE-$a.bin"
  cargo run -q -p mkimage -- build "$a" "$e" "$e" \
    "images/kernel-$PROFILE-$a.bin" "images/cibos-$PROFILE-$a.cimg" "$PROFILE"
  cargo run -q -p mkimage -- verify "images/cibos-$PROFILE-$a.cimg" "$a"

  echo "== $PROFILE / $a : CIBIOS firmware =="
  if [ "$FW_BARE" = "1" ]; then
    cargo build -p cibios --no-default-features --features "$FW_FEATURES" --target "$t"
    echo "  bare firmware: target/$t/debug/cibios  (complete bare boot pair)"
  else
    cargo build -p cibios --features "$FW_FEATURES"
    echo "  NOTE: $PROFILE requires Standard (signed) firmware, which is host-only"
    echo "        until the no_std SPHINCS+ verifier (SECURITY-NOTES.md). The kernel"
    echo "        image is built and the Standard firmware is host-verified; a bare"
    echo "        signed firmware is not yet available for this profile."
  fi
done

echo "Done: profile=$PROFILE — kernel images (and any bare firmware) under images/"
