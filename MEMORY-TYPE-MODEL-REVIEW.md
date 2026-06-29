# What is "device" vs "normal" memory — the real-world model, reviewed

The user's concern: is the device/normal baseline the actual modern-day standard,
or a made-up convenience? And how do devices/applications/firmware actually
register? Let me state the real model precisely.

## The ground truth: memory TYPE is a property of the PHYSICAL ADDRESS, set by the
## platform, not by software policy
On real hardware, every physical address belongs to a region with a fixed
character decided by how the SoC/board is wired:
  - RAM (DRAM): Normal memory — cacheable, the CPU may cache/reorder/prefetch.
    This is where code, stacks, heaps, page tables, and DMA buffers live.
  - MMIO (device registers): Device memory — must NOT be cached/reordered/gathered,
    because each access has side effects (reading a UART data register CONSUMES a
    byte; writing a control register triggers hardware).
  - ROM/flash, framebuffers, etc. have their own characters.

The OS does not INVENT which is which — it DISCOVERS it and maps each page with the
matching memory type. The authoritative source of the layout is:
  - aarch64 / riscv64: the DEVICE TREE (DTB) the firmware passes — every device
    node has a `reg = <base size>` and the /memory node gives RAM. (Or ACPI on
    servers.)
  - x86: the E820/UEFI memory map (RAM vs reserved vs MMIO) + PCI enumeration
    (BARs give device MMIO addresses). The legacy areas (VGA 0xB8000, etc.) are
    architecturally fixed.

So "Normal vs Device" is NOT a policy baseline we choose — it is a FACT about each
physical region that we must read from the platform and honor. The baseline
"everything is Normal unless explicitly device" is therefore WRONG in spirit: the
correct model is "each region gets the type the platform says it has." In practice
RAM is the large majority by area, and device MMIO is a set of comparatively small
windows — which is why mapping RAM Normal and carving device windows is the right
SHAPE — but the device windows must come from the platform (DTB/PCI), not a guess.

## How things "register" (the user's framing, made precise)
- FIRMWARE (CIBIOS, here): owns the very early platform. It enumerates RAM and
  devices (from the board, or by probing) and passes a handoff (+ DTB on
  ARM/RISC-V) describing the layout. WE control CIBIOS, so on real hardware CIBIOS
  is the authoritative source; in QEMU self-boot the DTB QEMU provides stands in.
- BOOT-TIME devices: present at power-on (UART, interrupt controller, timer,
  storage, NIC). Discovered from the DTB/ACPI/PCI enumeration at boot. These are
  the ones the MMU phase must map as Device before/at MMU-on.
- HOT-PLUG / post-boot devices: a device that appears after boot (USB, hot-plug
  PCIe) is discovered by the relevant bus driver, which then maps ITS BARs as
  Device on demand (after the MMU is online) — not part of the early identity map.
- APPLICATIONS: do NOT register MMIO. Apps run in ring-3/EL0/U-mode with NO device
  access; they use Normal user memory and reach hardware only via syscalls/the
  Lattice. So "applications register" = they register with the KERNEL (boundaries,
  channels), NOT with the page-type map. Application memory is always Normal, user.

## Verdict on the current code vs the standard
- SHAPE is right: RAM Normal (bulk) + carve device windows as Device.
- BASELINE wording was sloppy: the truth is "type follows the platform's region
  character", not "default Normal, opt-in Device". For RAM and app memory Normal is
  correct; for any MMIO the source must be the platform map (DTB/PCI), and right
  now aarch64's device window is a STANDARD-LAYOUT CONSTANT, not yet DTB-derived —
  that is the remaining gap to make it truly real-world.
- CAP=16 fixed arrays (registry + carve list) SILENTLY DROP extra regions — wrong
  for a real board that may expose many device nodes. Must not silently drop:
  either size to the real device count from the DTB, or fail LOUDLY, never silently.

## The correct end-state (modern standard)
1. Parse the DTB device nodes (and on x86, the E820 map + PCI BARs) to get the
   authoritative RAM regions (Normal) and device regions (Device).
2. Identity-map (or higher-half map) RAM as Normal, every device region as Device —
   driven entirely by discovered data, capacity bounded by the actual node count,
   never a silent cap.
3. Post-boot/hot-plug devices map their BARs as Device on demand via their bus
   driver.
4. Apps never touch this; they get Normal user pages and syscalls only.

---

## FIXED this session (per the review above)
1. SILENT FIXED-ARRAY CAP removed. The carve-out previously used a fixed
   [(u64,u64); 16] array that SILENTLY DROPPED the 17th+ device region (-> mapped
   Normal cacheable, the defect). Now the MMU phase collects device regions into a
   heap Vec (heap is online well before this phase) — NO cap, NO silent drop.
2. DYNAMIC REGISTRY now carved + Device-mapped UNIFORMLY. Previously only the
   static P::mmio_identity_ranges() was carved from Normal; the DTB-discovered
   registry was consulted separately and SKIPPED any region "already covered by the
   identity map" — leaving discovered low devices Normal cacheable. Now ALL device
   regions (static arch windows + dynamic registry) are merged (sorted, coalesced)
   into one set, carved out of the Normal identity map, and mapped Device. One
   coherent path on every arch.
3. REGISTRY OVERFLOW is now LOUD, not silent: register() returns bool; register_mmio
   warns via kprintln if the registry is full. CAP raised 16 -> 32 with rationale
   (early boot discovers a bounded small set; post-MMU/hot-plug devices map their
   own BARs on demand and do not use this early registry).
4. mmio_registry ungated from aarch64-only -> available on all arches (read by the
   MMU phase everywhere; populated by aarch64 today, x86 PCI BARs / riscv64 PLIC as
   their discovery is wired). Honest #[allow(dead_code)] where not yet populated.

VERIFIED: all 3 arches build clean + boot (aarch64/riscv64 MMU online + boot
complete; x86 full stack STACK OK + REMOTE LINK OK + isolation). 378 tests pass.
The aarch64 UART still works — proving the unified Device mapping is correct (it is
in the carved Device set).

## On the BASELINE question (answered)
"Normal vs Device" is NOT a chosen policy default — it is the platform's fact about
each physical region, which the kernel discovers (DTB/PCI/E820) and honors. RAM and
all application memory are Normal; device MMIO is Device. Apps NEVER register MMIO
(ring-3/EL0, syscalls only). Firmware (CIBIOS, which we control) is the
authoritative source on real hardware; the DTB stands in under QEMU self-boot. The
shape (Normal RAM bulk + carved Device windows) is the modern standard; the
remaining step to be fully real-world is to DERIVE the device windows from the DTB
for aarch64/riscv64 (x86 from PCI BARs) rather than the current standard-layout
constants — the discovery+registry plumbing for that now exists and is honored.
