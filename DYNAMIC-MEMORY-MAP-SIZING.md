# Deriving the identity map + reservation from discovered RAM (not QEMU constants)

## The QEMU-era shortcut found (dwelling on the dynamic MMIO work)
The dynamic MMIO mapping was correct, but dwelling on it exposed a DEEPER QEMU
assumption it sits on: identity_map_bytes and reserved_below are HARDCODED to QEMU
virt's layout:
  - aarch64: identity_map_bytes = 1.25 GiB, reserved_below = 1 GiB + 32 MiB —
    tuned to "RAM at 1 GiB, kernel at 0x40080000".
  - riscv64: identity_map_bytes = 2.25 GiB, reserved_below = 2 GiB + 32 MiB —
    tuned to "RAM at 2 GiB (OpenSBI low), kernel above".
On a REAL board (RAM at 512 MiB, or 32 GiB, different size) these constants are
simply WRONG and the kernel would not boot. The RAM region is ALREADY discovered
from the DTB (dtb_ram_region); these values must be DERIVED from it, not constants.

## The fix — derive from the discovered RAM region
The MMU phase already has the RAM region (it builds the frame allocator from the
handoff regions, which are DTB-derived). Compute:
  - identity_map_bytes: cover 0 .. (ram_base + ram_size), so all device MMIO (low)
    AND all RAM is identity-mapped. Page-count is bounded by real RAM size, not a
    guessed constant. (The discovered-MMIO loop already maps any device ABOVE this,
    so high device space is still covered.)
  - reserved_below: ram_base + KERNEL_SPAN, where KERNEL_SPAN covers the kernel
    image + heap + stack loaded at the RAM base. Derived from the actual load, not
    a hardcoded watermark. Frames are then drawn from above the kernel within real
    RAM.

## The contract change (minimal, no drift)
ArchPaging's identity_map_bytes()/reserved_below() are fn() -> u64 with no platform
input. Two clean options:
  (A) Pass the discovered RAM region (base,size) into the MMU orchestration and let
      it compute the map extent + reservation generically, with the arch hooks
      providing only the KERNEL_SPAN (image+heap+stack size) and any arch minimums.
  (B) Give the hooks the RAM region as a parameter: identity_map_bytes(ram) etc.
Option A is cleaner: the SIZING logic (cover 0..ram_end; reserve ram_base+span) is
PORTABLE — identical on every arch — so it belongs in the shared orchestration, and
the arch only supplies its kernel span (a small constant) + encoder/register ops.
This is the same shared-logic/per-arch-hook discipline as the rest of the MMU.

## What stays per-arch (legitimately)
- KERNEL_SPAN (how much to reserve above ram_base for image+heap+stack): depends on
  the kernel's load offset + heap size, which is arch/linker-specific but a small
  constant, not a platform address.
- x86_64: keeps its existing behavior (RAM effectively at 0; the PC layout). Its
  KERNEL_SPAN/map are the current KERNEL_IDENTITY_MAP_BYTES — unchanged.

## Result
The page tables' EXTENT is derived from the platform's real RAM (from the DTB), so
the kernel maps exactly what the board has — boots on QEMU virt AND a real board
with RAM anywhere. No compiled-in QEMU-virt size remains in the aarch64/riscv64
memory map. This is the bare-metal-correct sizing.

---

## DONE — verified
Implemented Option A. ArchPaging now exposes kernel_span() + min_identity_map_bytes()
(per-arch CONSTANTS relative to the RAM base, not platform addresses); the
orchestration derives the actual geometry from the DISCOVERED RAM region:
  - ram_base / ram_end computed from the (DTB-derived) usable regions.
  - reserved_below = ram_base + kernel_span()  (was a hardcoded watermark).
  - identity_map_bytes = max(min_identity_map_bytes(), ram_end)  (was a hardcoded
    window). The discovered-MMIO loop maps any device above this; the skip uses the
    same identity_map_bytes, so it stays consistent.

Verified (map size is now COMPUTED, proving it's derived not hardcoded):
  - aarch64: 1152 MiB = max(1024 MiB floor, ram_end 1.125 GiB). Was 1280 MiB
    hardcoded. MMU online, boot complete.
  - riscv64: 2176 MiB = max(1024, ram_end 2.125 GiB). Was 2304 MiB hardcoded. MMU
    online, boot complete.
  - x86_64: 1024 MiB (floor dominates; RAM ends within 1 GiB). Full stack intact
    (STACK OK, REMOTE LINK OK, boot complete). Behaviour preserved; the only change
    is reserved_below is now ram_base(0x100000)+64MiB = 0x4100000 (was flat
    0x4000000) — the kernel at 16 MiB is still cleared; benign and MORE correct
    (relative to real RAM base, consistent with the other arches).
  - 375 tests pass; all 3 arches build clean.

## Result
No hardcoded QEMU-virt memory-map size remains on any arch. The page-table extent
and the frame reservation are DERIVED from the platform's real RAM (read from the
DTB), so the kernel maps exactly what the board has and boots wherever RAM sits.
The per-arch piece is only kernel_span (a small image+heap+stack constant) — not a
platform address. Same shared-logic / per-arch-hook discipline as the rest of the
MMU work. This was the QEMU-era shortcut the dynamic-MMIO work was sitting on; it
is now removed.
