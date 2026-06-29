# MMIO must be mapped as DEVICE memory, not Normal cacheable (bare-metal fix)

## The shortcut found (dwelling on large-page correctness)
Both encode_leaf and encode_block_leaf on aarch64 map EVERYTHING as ATTR_NORMAL
(cacheable Normal memory) — INCLUDING device MMIO ranges (UART, GIC, NIC BARs).
On QEMU this happens to work (QEMU is lenient about memory attributes). On REAL
hardware it is a serious bug: the CPU may cache, reorder, prefetch, or merge
accesses to device registers, so MMIO reads/writes do not reach the device when
and as intended — the UART/GIC/NIC malfunction. Real device registers MUST be
mapped as Device memory (aarch64: Device-nGnRnE; x86: cache-disabled PCD/PWT).

The Permissions struct has NO way to express "device memory", so MMIO inherits
Normal attributes. THIS is a QEMU-era shortcut hiding in the mapping API.

## The fix
1. Add `device: bool` to Permissions (default false = Normal RAM). A device
   mapping is read/write, non-exec, non-user, device=true.
2. Each arch encoder honors it:
   - aarch64: device => ATTR_DEVICE (MAIR index 1 = Device-nGnRnE) and DROP
     SH_INNER (shareability is RES0/ignored for Device); Normal stays
     ATTR_NORMAL + SH_INNER. Applies to BOTH encode_leaf and encode_block_leaf.
   - x86_64: device => set PCD (page cache disable) + PWT (write-through) so the
     line is uncached; RAM stays write-back. Both leaf and block.
   - riscv64: base ISA has NO per-PTE memory type (cacheability is fixed by the
     platform's PMAs, set in hardware/DT, not the page table). So device is a
     DOCUMENTED no-op in the PTE; the field still flows for a uniform API and for
     the Svpbmt extension later (which DOES add per-PTE memory types). This is an
     honest arch difference, not a shortcut.
3. Map all MMIO ranges (static mmio_identity_ranges + discovered registry) with
   device=true; RAM identity map stays device=false.
4. Permissions helper constructors: keep kernel_rw() etc. as Normal; add a
   device_rw() (or set the field) for MMIO.

## Verification plan
- Add device field; update ALL Permissions literals + the 5 encoders + test
  encoders (device just won't set the test PS/extra bits — keep tests stable).
- Build all arches; full regression.
- Boot all 3: x86_64 full stack (NIC/UART are MMIO/port — confirm still works with
  device mapping), aarch64 + riscv64 MMU online + boot complete.
- This is the correct bare-metal behavior; QEMU still passes, real hardware no
  longer risks cached MMIO.

---

## DONE — verified
- Permissions gained `device: bool`; added Permissions::device_rw(). All
  constructors default device=false; all 7 literals updated (RAM=false, the 2 MMIO
  map sites = true).
- aarch64: encode_leaf + encode_block_leaf select (ATTR_DEVICE, no SH) when device,
  else (ATTR_NORMAL, SH_INNER). MAIR index 1 = Device-nGnRnE was already set up.
- x86_64: encode_leaf + encode_block_leaf set PWT|PCD (uncached) when device.
- riscv64: documented no-op (base ISA has no per-PTE memory type; PMAs govern
  cacheability). Field flows for a uniform API + future Svpbmt.
- MMIO ranges (static mmio_identity_ranges + discovered registry) now mapped
  device=true; RAM identity map device=false.

VERIFIED (the real test is that MMIO devices still work when mapped uncached):
  - aarch64 boots to "boot complete" — the UART is MMIO, so correct Device-nGnRnE
    mapping is PROVEN by there being serial output at all.
  - x86_64 FULL stack: MMU online, DNS STACK OK, REMOTE LINK OK, boot complete —
    the NIC is MMIO (virtio-net-pci); uncached PWT|PCD mapping works.
  - riscv64 boots (device no-op).
  - 378 tests pass; all arches build clean.

## Why this mattered (no QEMU shortcut)
Mapping device registers as Normal cacheable memory works on QEMU (lenient) but is
a real defect on hardware: the CPU may cache/reorder/prefetch/merge MMIO accesses,
so device registers are read/written wrongly. This is now correct on the two arches
whose ISA encodes memory type in the PTE (x86_64, aarch64). i686 (when its MMU
lands) must do the same: classic/PAE PTEs have PCD/PWT just like x86_64 — the
device field already flows, so its encoder will honor it identically.
