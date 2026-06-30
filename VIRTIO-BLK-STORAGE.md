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

---

## RISC-V DMA cache maintenance — the non-coherent path implemented
The coherency check surfaced that riscv64 virt is NOT dma-coherent. Implemented the
honest fix rather than leaving it a warning:
  - cibos-dtb gained find_prop_u32 (read tree-global scalars like
    riscv,cbom-block-size).
  - New kernel-image/src/arch/cache_riscv64.rs: Zicbom cache maintenance —
    clean_range (CBO.CLEAN, write back before the device reads) and
    invalidate_range (CBO.INVAL, after the device writes, before the CPU reads).
    The CBO instructions are emitted via raw `.insn` encoding so they assemble on
    the base target; they are NO-OPS unless the DTB-reported cache-block size has
    been configured (never a silent assumed value).
  - At boot, when virtio-mmio is non-coherent, the driver reads
    riscv,cbom-block-size from the DTB; if present it ENABLES maintenance (logged),
    else it WARNS that DMA is unsafe (no Zicbom mechanism) — it never silently
    proceeds unsafely.
  - The virtio-blk request path now cleans the header/ring/(write data) before
    NOTIFY and invalidates the ring/status/(read data) after completion, gated to
    riscv64 and no-op on coherent platforms.

VERIFIED:
  - riscv64 virt: "non-coherent — Zicbom cache maintenance enabled (block size 64
    bytes)"; virtio-blk read LBA 0 returns OK with the CBO instructions executing.
  - aarch64: coherent, no maintenance (silent); reads OK.
  - 381 tests pass; all 3 build clean.
HONESTY: QEMU implements Zicbom AND models coherent memory, so the CBOs are valid
and execute but their staleness-prevention effect is not observable here — on real
non-coherent RISC-V hardware these are what make DMA correct. The mechanism is
correct-by-construction and gated on real DTB facts; full effect needs non-coherent
silicon to validate.

## virtqueue index arithmetic — stress-verified
A 300-sequential-read stress test (ring size 128, so the avail/used indices wrap)
returned 0 failures, confirming the free-running 16-bit counter + ring-slot modulo
arithmetic is correct across wraps (single-in-flight, lock-serialized). Test was
temporary; removed after passing.

---

## Regression fix: DTB node order is NOT address order (real bare-metal bug)
The no-disk riscv64 case faulted (cause 5 load access fault at 0x10009000) after the
cache-maintenance increment. Root cause was a latent bug the no-disk path exposed:
  - The probe walks the virtio-mmio slot array UPWARD from a base, for `count` slots.
  - The base came from device_reg("virtio_mmio") = the FIRST matching DTB node.
  - But QEMU's riscv64 virt lists virtio_mmio@ nodes in DESCENDING address order, so
    the "first" node is @10008000 (the TOP of the array), not @10001000. Walking up
    from there ran past the array end (0x10009000) and faulted.
  - It was masked with a disk because the probe `break`s on finding the device.
FIX (no QEMU shortcut — discover the real layout):
  - cibos-dtb gained device_reg_lowest(prefix): the LOWEST-addressed matching node,
    and count_nodes(prefix): the real number of matching nodes.
  - The kernel registers the slot window from the lowest base and walks exactly
    `count_nodes` slots (aarch64 virt = 32, riscv64 virt = 8) — both from the DTB,
    no hardcoded count or assumed ordering.
VERIFIED:
  - riscv64 no-disk: boots ("no virtio-blk device... skipping"); with-disk: finds
    the device at its true slot (slot 7 = 0x10008000) and reads LBA 0 OK.
  - aarch64 both cases OK; x86 unaffected. 382 tests (+1 device_reg_lowest test
    asserting base 0x10001000 + 8 slots on the real riscv DTB).
LESSON: never assume DTB node order corresponds to address order — enumerate and
compute (min base, real count) from the actual tree.

---

## CIBOSFS mounted on virtio-blk (aarch64/riscv64 root filesystem)
The full filesystem stack now runs on the virtio-blk device, the same
arch-independent Fs<D: BlockDevice> layer x86 uses on ATA — only the device differs.
In verify_virtio_blk (after the block-layer LBA 0 check) the kernel:
  - Fs::format(disk, 64) — lay down a fresh CIBOSFS.
  - mkdir(/etc), write_file(/etc/hello, ...), read_file — round-trip a file.
  - into_device() + Fs::mount() — REMOUNT and re-read to prove the data is on the
    medium, not just in RAM.
VERIFIED: aarch64 (slot 31) and riscv64 (slot 7) both print
"CIBOSFS on virtio-blk — format/write/read-back/remount OK". This also exercises
the riscv64 Zicbom cache-maintenance path under real multi-sector filesystem I/O
(not just a single sector). No-disk degrades gracefully; x86 ATA path unaffected;
383 tests pass.

NOTE (honest scope): this proves the fs-on-virtio-blk path end to end. Installing
it as the persistent kernel ROOT_FS so the filesystem SYSCALLS operate on it
(aarch64/riscv64, mirroring x86's ROOT_FS<Fs<AtaDisk>>) is the next step — it needs
either a per-arch ROOT_FS static typed Fs<VirtioBlkMmio> or a dyn-BlockDevice
refactor of the ROOT_FS type; deferred to keep this increment focused and verified.

## DTB walker hardening (real bare-metal crash fix)
A truncated/corrupt DTB made prop_name index past the strings block and PANIC — a
malformed blob from firmware would crash the kernel during device discovery. Fixed
prop_name to bounds-check (empty name on overflow). Audited all walker slicing: the
node-name slices are bounded by the struct-block loop; be32/be64 and reg-data use
.get() (Option). Added a truncated-blob test exercising count_nodes/
device_reg_lowest/find_prop_u32/node_has_prop — all safe now. 383 tests pass.
