# Dynamic MMIO mapping — discovered devices drive the page tables

## The defect (real-hardware, not QEMU)
We discover the UART base from the DTB (and will discover GIC/PLIC/etc.). But the
MMU phase only mapped a STATIC list (mmio_identity_ranges) plus the low RAM map.
On QEMU virt the UART (0x09000000) happens to fall inside the 1.25 GiB low map,
so putc keeps working after the MMU comes online. On a REAL board whose UART (or
other discovered device) sits OUTSIDE the RAM identity map, putc would fault the
instant the MMU turned on. Documenting that as an edge case is a QEMU-era
shortcut. For bare metal we FIX it: whatever the DTB reports must be mapped.

## The design — a runtime MMIO registry
Real kernels map device register space they DISCOVER. So:
  - A small portable runtime registry (mmio_registry) holds (base, size) ranges
    that device discovery records. cibos-kernel-agnostic; lives in boot.rs.
  - When the kernel discovers a device from the DTB (UART now; GIC/PLIC later) it
    calls register_mmio(base, size) — recording the page-rounded region.
  - The MMU phase maps EVERY registered range (page-rounded: floor(base) ..
    ceil(base+size)) in addition to RAM and the static arch ranges. So the page
    tables follow discovery: any real-board device address is mapped, wherever it
    is. No board-specific assumption.
  - Idempotent / overlap-safe: ranges already covered by the RAM identity map are
    skipped (map_range errors on already-mapped pages), so we only map what's not
    already mapped. We check containment against the identity map extent before
    mapping, and dedup the registry.

## Ordering (the subtlety)
UART is discovered EARLY (before MMU) so early kprintln works via the bootstrap
default, then the override. The registry is populated at discovery time (early),
and the MMU phase (later) reads it — so by the time the MMU builds tables, every
discovered device is in the registry and gets mapped. Devices discovered AFTER
the MMU is online (none today) would need an explicit map call; flagged for when
that arises.

## Page rounding
A device base need not be page-aligned. Map from floor(base/4096)*4096 for
ceil((offset_in_page+size)/4096) pages, so the whole register window is covered.

## Result
The page tables are driven by what the platform actually has (RAM from DTB +
devices from DTB), not by compiled-in QEMU-virt ranges. This is the bare-metal
dynamic-scenario handling: read everything from the DTB, map everything
discovered, no shortcuts.

---

## DONE — verified
- mmio_registry (boot.rs, aarch64-scoped): fixed-capacity (CAP=16) runtime
  registry of page-rounded discovered MMIO regions; register_mmio(base,size)
  populates it at discovery time, usable before heap/MMU exist (atomics, no lock).
- kernel_entry registers the discovered UART window (DTB pl011, or the bootstrap
  default if no DTB) so it is mapped wherever the board places it.
- bring_up_mmu_generic maps every registered region that falls OUTSIDE the RAM
  identity map (regions inside it are already mapped — skipped). Page-rounded.
- Verified: QEMU PL011 0x09000000 rounds to 0x9000000..0x9001000, inside the
  1.25 GiB map -> correctly SKIPPED; a real high/unaligned UART (e.g. 0xFF010A00)
  rounds to 0xFF010000..0xFF012000, outside the map -> correctly MAPPED. aarch64
  boots, MMU online, boot complete; x86_64 + riscv64 unaffected; 375 tests pass.
- This is the bare-metal fix (not a documented edge case): discovery drives the
  page tables, so any real-board device address is mapped.

## Also this session: seed_entropy now portable on all arches
KERNEL_RNG + seed_kernel_rng were x86-gated but are pure cibos_kernel code (no arch
specifics). Ungated them; extracted seed_entropy_portable(seed); all four
ArchBringUp impls now call it. aarch64/riscv64 seed_entropy went from
Skipped("pending RNG") to Done — the CSPRNG is seeded on every arch via identical
shared code. Verified: the "entropy seed skipped" line is gone on aarch64/riscv64;
x86 full stack unchanged; 375 tests pass.
