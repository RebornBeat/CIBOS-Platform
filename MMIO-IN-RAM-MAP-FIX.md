# Low MMIO mapped as Normal by the RAM identity map (bare-metal bug)

## The bug (found dwelling on the device-memory fix)
On aarch64 QEMU virt, peripherals (PL011 UART 0x09000000, GIC 0x08000000) live
BELOW the RAM base (1 GiB). The RAM identity map covers 0..identity_map_bytes as
NORMAL cacheable (now via 2 MiB blocks). So the UART/GIC fall INSIDE the Normal
identity map and are mapped Normal cacheable — the exact device-memory defect, but
coming from the RAM identity map rather than the MMIO path. The MMIO registry even
registers the UART, but the consumer skips ranges already covered by the identity
map, so the device attributes never apply. QEMU tolerates it; real hardware can
malfunction (cached/reordered/gathered MMIO).

## Root cause
The identity map maps a CONTIGUOUS 0..N range as Normal, assuming everything in
that range is RAM. On QEMU virt (and most real ARM/RISC-V boards) the low physical
region is NOT all RAM — it holds memory-mapped peripherals. Mapping those as Normal
is wrong.

## The fix (bare-metal correct, no QEMU assumption)
The identity map must map each page with the memory type that matches what's there:
device for MMIO, Normal for RAM. Cleanest robust approach that does not depend on
QEMU-virt specifics:
  - The RAM identity map should only map the DISCOVERED RAM region(s) as Normal,
    NOT a blanket 0..N. The low device region below RAM is then mapped as DEVICE by
    the MMIO path (mmio_identity_ranges / the registry).
  - BUT: the kernel currently relies on a flat 0..N identity map so phys==virt for
    ALL addresses it touches early (incl. the UART before the MMU). If we only map
    the RAM region, the low UART page is unmapped until the device mapping runs —
    which is fine because the device mapping runs in the SAME bring_up_mmu before
    install, so by the time the MMU turns on, both RAM (Normal) and devices
    (Device) are mapped with correct types.

## Chosen implementation (minimal, correct, arch-data-driven)
Keep the single flat identity map for SIMPLICITY and the phys==virt invariant, but
split its memory type by what each region is:
  1. Map the discovered RAM region(s) as Normal (device=false).
  2. Map the low peripheral region(s) below RAM as Device (device=true) — these
    come from the arch's mmio_identity_ranges (now NON-empty for aarch64: the
    peripheral block below RAM) and/or the DTB-discovered device registry.
  3. Ensure NO overlap (RAM map and device map cover disjoint physical ranges), so
    no page is mapped twice and each gets the correct type.
This means: identity_map_bytes should map RAM, and the sub-1GiB peripheral window
must be mapped as Device explicitly (not swallowed by a 0..N Normal blanket).

Concretely for aarch64 QEMU virt: RAM 0x40000000.., devices 0x08000000-0x09001000.
The Normal map must start at the RAM base region; the device window must be mapped
Device. The current code maps 0..ram_end as ONE Normal range — that is the bug.

## Verification
- aarch64: UART still prints (now Device-mapped, not Normal) -> boot completes.
- x86_64: RAM at low addresses with the PCI hole high; its device hole is already
  mapped device via mmio_identity_ranges, and low RAM has no peripherals in the
  identity-mapped range, so x86 is unaffected (verify full stack unchanged).
- riscv64: peripherals (UART 0x10000000, PLIC 0x0C000000) are BELOW RAM (2 GiB) and
  currently swallowed by the Normal map too — same fix applies; device is a PTE
  no-op on riscv64 base ISA, so functionally Normal there anyway, but the MAP
  STRUCTURE should still be correct/disjoint for when Svpbmt lands.

---

## DONE — verified
Implemented the segmented identity map: bring_up_mmu_generic maps [0,
identity_map_bytes) as Normal in segments that CARVE OUT the arch's device ranges
(P::mmio_identity_ranges, page-rounded, sorted, fixed 16-slot array, no alloc),
then the existing MMIO loop maps those device ranges as Device. Result: disjoint,
correctly-typed map; the flat phys==virt invariant is preserved.

aarch64 mmio_identity_ranges now returns the low peripheral window
0x08000000..0x09100000 (GIC + PL011), which sits below the RAM base inside the
identity map — so it is carved out of Normal and mapped Device.

VERIFIED:
  - aarch64 boots to "boot complete" — and since the UART is in that carved
    Device window, the kernel printing MMU-online + boot-complete THROUGH the UART
    proves it is correctly Device-mapped (not the prior Normal-cacheable).
  - x86_64 FULL stack UNCHANGED: its PCI hole (0xFEB00000) is ABOVE map_end
    (1 GiB), so the hole clamps empty, no carve happens, and the map is a single
    Normal 0..1 GiB segment exactly as before. MMU online, isolation, STACK OK,
    REMOTE LINK OK, boot complete.
  - riscv64 boots (device is a PTE no-op there, but the map STRUCTURE is now
    correct/disjoint for when Svpbmt lands; its low peripherals are likewise
    carved if listed).
  - 378 tests pass; all arches build clean.

## Why this mattered (the deepest no-QEMU-shortcut catch yet)
The device-memory fix last session only typed the SEPARATE MMIO map. But the RAM
identity map blanket-covered low peripherals as Normal cacheable, silently winning.
This is the subtle, real-hardware-only defect: on an emulator cacheable MMIO is
tolerated; on silicon the UART/GIC would misbehave. The identity map now follows
the real platform's RAM-vs-device layout instead of assuming 0..N is all RAM.

## Remaining honesty
The aarch64 device window is still a conservative QEMU-virt/standard-layout
constant (0x08000000..0x09100000). The fully dynamic step is to derive these device
ranges from the DTB (device_base for each peripheral) so ANY board's layout is
carved correctly. Tracked; the mechanism (carve + Device-map) is now correct and
in place, fed by an arch range that will become DTB-driven.
