#!/bin/sh
# Build the CIBIOS firmware for 32-bit x86 (legacy BIOS, older hardware).
#
# 32-bit bare-metal x86 has no stable precompiled `core`, so this uses a custom
# target spec plus `-Z build-std` on the nightly toolchain. Everything else in
# the workspace builds on stable; only this one firmware variant needs nightly.
set -e
. "$HOME/.cargo/env" 2>/dev/null || true
TGT="$(cd "$(dirname "$0")" && pwd)/targets/i686-cibos-none.json"
cargo +nightly build -p cibios \
  --target "$TGT" \
  -Z json-target-spec \
  -Z build-std=core,alloc,compiler_builtins \
  -Z build-std-features=compiler-builtins-mem "$@"
echo "Built: target/i686-cibos-none/debug/cibios (ELF32 i386)"
