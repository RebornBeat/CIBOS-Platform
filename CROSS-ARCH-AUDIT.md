# Cross-arch finalization audit (x86_64 / aarch64 / riscv64)

A systematic confirmation that EVERY update from the MMU-generalization arc is
applied consistently on all three implemented arches, with no gaps or breakage.
(i686 is tracked separately in I686-PARITY-ACCOUNTING.md — it does not yet have a
PageTableEncoder/MMU.)

## Audit 1 — PageTableEncoder completeness
All 6 methods (encode_table, encode_leaf, encode_block_leaf, is_present,
is_block_leaf, entry_frame) present on all 3 arch encoders + both test encoders.
(Compiler-enforced; confirmed explicitly.)  PASS

## Audit 2 — Device memory honored in BOTH 4 KiB and 2 MiB paths
- x86_64: encode_leaf + encode_block_leaf both set PWT|PCD (uncached) on
  perms.device.  PASS
- aarch64: both select (ATTR_DEVICE, sh=0) on perms.device, (ATTR_NORMAL,
  SH_INNER) otherwise. MAIR_EL1=0x00FF => Attr0=Normal, Attr1=Device-nGnRnE;
  encoder AttrIndx 0/1 match.  PASS
- riscv64: documented no-op (memory type from PMAs, not PTEs; Svpbmt noted for
  future). Field flows for a uniform API.  PASS

## Audit 3 — Unified device-region carve+map (no cap, static+dynamic)
- Heap Vec, NO fixed cap.  PASS
- Collects BOTH static P::mmio_identity_ranges() AND dynamic mmio_registry.  PASS
- Sorted + coalesced; Normal RAM mapped in segments carving out device regions;
  every device region then mapped Device. Disjoint by construction.  PASS
- Runs in shared bring_up_mmu_generic<P> — NOT arch-gated; all 3 arches use it. PASS
- Device regions ABOVE map_end (x86 PCI hole 0xFEB00000) still Device-mapped by
  step (2) (no clamp there).  PASS
- Every error path fails LOUDLY (kprintln + return), never silently.  PASS

## Audit 3d — x86 NIC validation
x86 PCI hole now Device(uncached)-mapped via the unified path; full stack boots,
DNS STACK OK + REMOTE LINK OK => uncached NIC MMIO correct.  PASS

## Audit 4 — Registry: no silent drop
register() returns bool; full registry => false => register_mmio warns LOUDLY.
CAP=32 documented as generous early-boot headroom. (Fixed a STALE doc comment that
still said "silently ignores".)  PASS

## Audit 5 — Registry gating honesty
register_mmio is aarch64-only (the only arch with DTB UART discovery wired today);
registry mod is ungated (read by all arches' MMU phase) with #[allow(dead_code)]
and a comment that x86 (PCI BARs) / riscv64 (PLIC/CLINT) will populate it as their
discovery is wired. Honest "not yet", not a hidden gap.  PASS

## Audit 6 — Derived geometry (no hardcoded QEMU map size)
All 3 arches implement kernel_span() + min_identity_map_bytes(); shared orchestration
computes reserved_below = ram_base + kernel_span and identity_map_bytes =
max(floor, ram_end) from DISCOVERED RAM. Booted sizes: aarch64 1152 MiB, riscv64
2176 MiB — computed, not hardcoded.  PASS

## Audit 7 — Entropy + bring-up contract
All 4 seed_entropy impls route to seed_entropy_portable (full 32-byte seed). PASS
Remaining Skipped phases are honestly labeled: mount_root_fs/verify_storage pending
block drivers (aarch64/riscv64/i686). These are genuine remaining WORK, not gaps.

## Audit 8 — Definitive gate
378 workspace tests passing, 0 failing. All 3 arches build clean (0 warnings) and
BOOT (x86 full stack all milestones; aarch64/riscv64 MMU online + boot complete).

## CONCLUSION
All updates from the MMU-generalization arc (ArchBringUp contract; shared MMU
orchestration; DTB RAM + UART discovery; derived geometry; portable entropy;
large-page 2 MiB blocks with block-aware translate + aliasing guard; device-memory
correctness; unified no-cap device-region carve/Device-map) are APPLIED and
CONSISTENT across x86_64, aarch64, riscv64, with nothing broken and no gaps. The
only honestly-remaining items are per-arch driver phases (storage/NIC/ring-3) and
fully DTB-deriving the device windows (mechanism in place, currently standard-layout
constants for the static ranges).

---

## Follow-up (next session): a gap the audit had ASSERTED away — now fixed
The audit said "no gaps," but dwelling on the unified device-map's boundary math
found a real one: the derived `ram_end` / `identity_map_bytes` / `map_end` were
NOT page-aligned. They come from the handoff/DTB as base+length; QEMU virt happens
to report 4 KiB-aligned RAM bounds, so it never bit — the classic QEMU-era
shortcut. On real firmware reporting an unaligned RAM length, the carve loop's
`seg_pages = (map_end - cursor) / FRAME_SIZE` would TRUNCATE and silently drop the
final partial page, leaving a sliver of RAM unmapped (a fault waiting to happen).

FIX: page-align the derived bounds explicitly —
    ram_end           &= !(FRAME_SIZE - 1);   // floor to a page
    identity_map_bytes  = (floor.max(ram_end)) & !(FRAME_SIZE - 1);
The identity map can only map whole pages, so flooring is correct; mapping must
never silently drop a partial page. With registry regions already page-rounded
(floor base / ceil end) and static device ranges hand-aligned, ALL inputs to the
carve/device-map division are now page-aligned, so every division is exact — no
truncation drop on any arch.

VERIFIED: byte-identical on QEMU (masking is a no-op there — aarch64 still 1152
MiB, riscv64 2176 MiB, x86 full stack all milestones); 378 tests pass. The fix
only changes behavior on a real board with unaligned RAM bounds — exactly the
bare-metal case the audit's "no gaps" had not actually tested.

LESSON: an audit that confirms by reading + booting on QEMU can still miss a
real-hardware boundary case. "No QEMU-era shortcut" means probing the values that
QEMU happens to make convenient (here: page-aligned RAM bounds) and not relying on
that convenience.
