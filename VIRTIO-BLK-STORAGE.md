# virtio-blk over virtio-MMIO — aarch64/riscv64 storage

The storage phase for the virtio-MMIO arches (aarch64/riscv64 `virt` and real
ARM/RISC-V boards): a real block device behind the existing BlockDevice trait,
the MMIO analogue of the x86 ATA driver.

## What it does (all real device accesses, no stub)
- virtio-MMIO transport: MagicValue/Version/DeviceID registers, the device-init
  handshake (reset -> ACK -> DRIVER -> features -> FEATURES_OK -> DRIVER_OK).
- A split virtqueue (descriptor table + avail ring + used ring) in one contiguous
  identity-mapped DMA region from the FrameAllocator.
- The virtio-blk request protocol: a 3-descriptor chain (16-byte header R, data
  buffer R/W, 1-byte status W), polled to completion on the used ring.
- Implements BlockDevice (block_count / read_blocks / write_blocks).

## Bare-metal correctness (no QEMU shortcut)
- The virtio-MMIO slot array base comes from the DTB (`virtio_mmio@` nodes),
  NOT a hardcoded address. The driver walks the slots and probes each for a block
  device (DeviceID == 2).
- The slot window is mapped Device (uncached) via the discovered-MMIO registry +
  carve flow.
- DMA COHERENCY IS VERIFIED, NOT ASSUMED: the polled driver orders CPU/device
  accesses with a memory barrier and does NO cache maintenance — correct ONLY on a
  coherent platform. The driver checks the virtio-mmio node's `dma-coherent` DTB
  property and WARNS LOUDLY if absent.
  REAL FINDING: aarch64 virt marks virtio-mmio `dma-coherent`; riscv64 virt does
  NOT. So on real RISC-V hardware the barrier-only path is insufficient and cache
  clean/invalidate around DMA is required. QEMU's TCG models coherent memory
  regardless, so this would be an invisible QEMU-era shortcut WITHOUT the check —
  now it is surfaced loudly. (Implementing the RISC-V cache-maintenance path is the
  honest follow-up; the warning makes the gap explicit rather than silent.)

## Contract reconciliation (honesty)
The probe runs inside bring_up_mmu (which owns the FrameAllocator the virtqueue DMA
needs), after the MMU is online (DMA addresses must be stable under the final
tables). Its outcome is recorded, and the verify_storage contract phase now reports
truthfully: Done when a disk was found+read OK, Skipped("no virtio-blk device
present") otherwise — no longer a blanket Skipped("pending block driver").
mount_root_fs reports "virtio-blk verified in MMU phase; root-fs mount pending" —
the driver exists; mounting CIBOSFS on it is the next step.

## Verified
- aarch64 (via the ARM64 Image, real DTB): virtio-blk online at a discovered slot,
  2048 sectors, LBA 0 read returns the exact 0x55AA signature written to the disk —
  a byte-accurate virtqueue DMA read.
- riscv64 (via OpenSBI DTB): same, plus the dma-coherent WARNING (correctly,
  riscv64 virt omits the property).
- No-disk case degrades gracefully ("no virtio-blk device in any slot").
- x86 full stack unaffected (its ATA path unchanged). 381 tests pass (+1
  dma-coherent detection test against both real DTBs); all 3 build clean.

## Next
- RISC-V cache maintenance for the non-coherent DMA path.
- Mount CIBOSFS on the virtio-blk device (mount_root_fs) so the same filesystem
  syscalls x86 uses work on aarch64/riscv64.
- write_blocks VERIFIED (temporary write+readback LBA 1 returned byte-identical data on aarch64); next: persist via CIBOSFS.
