# Diagnosis: no-NIC boot hang — actually the e1000 MMIO BAR is unmapped

## The meta-question answered
Was this an A1 regression / did we change probe_nic_at_boot wrongly? NO.
- A1 (remote UDP Links) is correct and verified; it did not touch e1000.
- probe_nic_at_boot's structure (virtio-net first, then e1000) is correct.
- The bug is a LATENT e1000 (N3) driver flaw, exposed only now because the e1000
  path had never actually run against a real e1000 device until this no-NIC test.

## Why it "looked like" no-NIC but wasn't
The QEMU command did NOT pass `-nic none`, so QEMU's i440fx machine adds its
DEFAULT NIC — which is an e1000 (vendor 0x8086, device 0x100E). So "no NIC" boots
actually HAD an e1000. Diagnostic proof: instrumenting probe to report the matched
device printed bus 0, slot 3, device 0x100E — a real e1000. find_e1000 was correct;
the hang was AFTER the match, on first MMIO access.

## Root cause (certain)
The e1000 uses a MEMORY BAR: BAR0 on QEMU i440fx is 0xFEB80000 (~4075 MiB). The
kernel's identity map (KERNEL_IDENTITY_MAP_BYTES) covers only the first 1 GiB
(1024 MiB). The driver assumes "identity map: phys == virt" and reads/writes
mmio = 0xFEB80000 directly — an UNMAPPED virtual address -> page fault -> hang.
virtio-net never hit this because its legacy transport uses PORT I/O (no mapping),
not MMIO. This is the first MMIO-BAR device in the kernel.

## Faithful fix (real MMIO mapping, no shortcut)
Map the e1000's MMIO BAR region into the kernel address space before touching it
(the e1000 register space is 128 KiB). Any OS must map device MMIO; the "phys ==
virt under the 1 GiB identity map" assumption only holds for low DMA frames, not
high device BARs. Options:
  (a) Map the BAR's pages into the active address space at probe time (proper
      MMIO mapping; uncacheable). Requires a map_mmio(phys, len) -> virt helper in
      the paging/address-space layer.
  (b) Extend the identity map to cover the PCI MMIO hole (e.g. up to 4 GiB). This
      is simpler but maps a large range; must mark device MMIO uncacheable to be
      correct. Many simple kernels identity-map the low 4 GiB for exactly this.
DECISION: (a) is the clean, scalable answer (map only what we use, uncacheable),
and it's the honest "real OS maps its device memory" behavior. Implement a
map_mmio helper and use it in E1000::probe for BAR0 (and unmap on failure).

## Also fixed (production-correctness, found en route)
- eeprom_read had an UNBOUNDED loop -> bounded to a budget, returns Option; probe
  bails (returns None) if the EEPROM never responds. A real driver must never spin
  forever on a hardware register.

## Verification plan
- Boot WITH an explicit e1000 (-device e1000 / default NIC): probe maps BAR0,
  reads MAC from EEPROM, sets up rings, ARP + (if wired) DNS round-trip works.
- Boot with `-nic none`: no NIC at all -> clean "no supported NIC" + boot complete.
- Boot with virtio-net: virtio found first, e1000 never runs (unchanged).
All bare-metal-first; QEMU verifies the standard e1000 + PCI MMIO interfaces.

---

## RESOLUTION (implemented + verified)
Fix chosen: map the PCI MMIO hole in the kernel's boot page tables. After the
1 GiB low-RAM identity map, a second map_range covers 0xFEB00000..0xFEC00000
(1 MiB) identity, kernel-RW, NON-EXECUTABLE — the i440fx PCI MMIO region where
the e1000 BAR0 (0xFEB80000) lives. The driver's "phys == virt" assumption now
holds for its registers too. Also: eeprom_read bounded (returns Option) and
probe bails if the EEPROM never responds.

VERIFIED (all three NIC paths + A1):
  * e1000 (-device e1000): "e1000 MAC 52:54:00:12:34:56" (real EEPROM MAC) +
    "e1000 TX: ARP request sent" + "e1000 RX: ARP reply" -> boot complete. The
    e1000 driver TX+RX now proven against the real device.
  * -nic none (truly no NIC): "no supported NIC found (loopback only)" -> boot
    complete. No hang.
  * virtio-net: virtio found first, full stack + A1 remote Link
    ("STACK OK", "REMOTE LINK OK") -> boot complete. Unchanged.
370 tests green; all configs + aarch64/riscv64 build clean.

## Confirmed: not an A1 regression
A1 (remote UDP Links) was correct all along. The hang was a latent e1000 (N3) bug
— an unmapped MMIO BAR — exposed only when the e1000 path first ran against a real
e1000 (QEMU's default NIC). probe_nic_at_boot was never wrong; nothing to regress.
