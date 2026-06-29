# Identity map: page-alignment (FIXED) + per-region awareness (scoped next)

## FIXED this session — derived bounds page-alignment
ram_end / identity_map_bytes / map_end were not page-aligned (base+length from the
handoff/DTB). QEMU virt reports aligned RAM so it never bit; real firmware may
report unaligned bounds, and the carve loop's integer division would silently drop
a partial page. Now floored to a page boundary; all carve/map division is exact.
Byte-identical on QEMU (1152/2176 MiB unchanged); protects the real-HW path.

## DWELT ON — flat [0, map_end) Normal map maps non-RAM holes as Normal
The Normal identity map currently covers [0, map_end) flat (minus device carves),
which includes:
  - the entire sub-ram_base space (aarch64 [0,1GiB), riscv64 [0,2GiB), x86 [0,1MiB))
    that is NOT RAM, and
  - any non-RAM HOLE between usable regions (real boards can have split RAM; QEMU
    virt reports a SINGLE contiguous region, so this is untested today).
The FrameAllocator already respects holes (only frees frames fully inside Usable
regions), so the kernel never HANDS OUT a hole frame. The imperfection is only that
the identity map TYPES holes as Normal rather than leaving them unmapped (so a
stray access would read garbage rather than fault loudly).

## Why NOT naively switched to per-region this session (would CORRUPT x86)
A per-region Normal map (only [ram_base, ram_end) per usable region) would UNMAP
everything below ram_base — but the x86 VGA text buffer at 0xB8000 is below
ram_base (1 MiB) and is NOT in mmio_identity_ranges. Today it works ONLY because
the flat low map covers it as Normal. Switching naively => x86 loses VGA output
(fault). The flat low map is currently load-bearing for x86 VGA.

## The correct, ordered path to per-region (no shortcut, no corruption)
1. Reclassify the sub-ram_base MMIO each arch genuinely needs as explicit device
   ranges:
     - x86: VGA 0xB8000 (text buffer — arguably device/uncached or at least
       explicitly mapped), plus anything else below 1 MiB the kernel touches
       post-MMU. (COM1 is port-I/O, not memory-mapped — no mapping needed.)
     - aarch64: GIC + UART already carved.
     - riscv64: PLIC/CLINT/UART (when wired) carved.
2. Switch the Normal map to iterate USABLE RAM regions ([ram_base_i, ram_end_i)),
   carving devices, instead of flat [0, map_end). This:
     - is byte-identical TODAY (one usable region => one segment), so it cannot
       corrupt the working path once VGA is reclassified;
     - maps only real RAM as Normal and leaves holes UNMAPPED (stray access faults
       loudly — the bare-metal-correct behavior);
     - is testable with a unit test using multiple regions + a hole.
3. Verify all 3 arches byte-identical (x86 VGA still works via its new device
   range), add the multi-region test, then ship.

This is a real bare-metal improvement (removes the "single contiguous RAM" QEMU
convenience), but it must be sequenced AFTER VGA reclassification or it regresses
x86. Doing it now without that would be the exact kind of rushed change that
corrupts a working path — so it is scoped as the next increment, not asserted away.

---

## Step 1 DONE — x86 VGA reclassified as an explicit device range
x86 mmio_identity_ranges now includes 0xB8000..0xB9000 (the VGA text buffer)
alongside the PCI hole. VGA is now an explicit, carved, Device(uncached) mapping
rather than incidentally covered by the flat low Normal map. Uncached is correct
for a framebuffer (writes go straight through; no stale cache).
VERIFIED: x86 full stack boots (MMU online, STACK OK, REMOTE LINK OK, boot
complete); a QEMU screendump shows 3132 non-zero pixel bytes — the VGA buffer is
genuinely written and rendered through the new mapping. 378 tests pass; all 3
arches build clean and boot.
This UNBLOCKS the per-RAM-region Normal map (step 2): with VGA no longer dependent
on the flat low map, the Normal map can be switched to iterate usable RAM regions
(leaving non-RAM holes unmapped to fault loudly) without regressing x86 output.
Step 2 remains the next increment (byte-identical today since the handoff reports a
single region; the win appears on real boards with split RAM).

---

## Step 2 DONE — per-RAM-region Normal map (the QEMU "single contiguous RAM" shortcut removed)
The Normal identity map now iterates the platform's USABLE RAM regions and maps
each [ram_base_i, ram_end_i) Normal (carving device ranges per region), instead of
a flat [0, map_end). Non-RAM holes — gaps between RAM banks AND the sub-ram_base
space that is not a declared device — are now left UNMAPPED, so a stray access
faults loudly instead of silently reading a Normal-cached phantom mapping.

Prerequisites completed first (so per-region does not unmap something needed):
  - x86 VGA (0xB8000) declared device (prior step).
  - riscv64 CLINT/PLIC/UART declared device this session (were relying on the flat
    map; now explicit + correctly Device-typed, forward-looking for their drivers).
  - aarch64 GIC/UART already declared.
Verified kernel image/stack/heap/page-tables all live within usable RAM regions on
all 3 arches (x86 load 16 MiB, aarch64 0x40080000, riscv64 0x80200000), so per-
region covers the executing kernel.

VERIFIED:
  - Byte-identical on QEMU (single RAM region => one set of segments): aarch64 1152
    MiB, riscv64 2176 MiB, x86 full stack all milestones incl. VGA render + the
    keyboard IRQ firing (proves the IDT is reachable => unmapping sub-1MiB low
    memory is safe).
  - SPLIT-RAM PROOF: temporarily injected a 2nd usable bank above a 64 MiB hole on
    aarch64. Result: frame allocator saw 40960 frames (bank1 + bank2), map extended
    to 1248 MiB covering BOTH banks, the hole left unmapped, MMU online + boot
    complete. The old flat map would have mapped the 64 MiB hole as Normal
    cacheable. Reverted after proving.
  - 378 tests pass; all 3 build clean.

RESULT: the identity map now follows the platform's real RAM layout (multi-bank,
arbitrary base) with every device window explicitly Device-typed and holes
unmapped — no remaining "single contiguous RAM from 0" QEMU-era assumption.
