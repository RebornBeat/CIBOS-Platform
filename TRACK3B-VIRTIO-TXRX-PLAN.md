# Track 3B — virtio-net TX/RX over the virtqueues (design, verified foundation)

## Foundation (verified this session)
- The driver (kernel-image/src/arch/virtio_net.rs) already does device discovery,
  PCI enable, the legacy virtio init handshake, MAC + link read. It is PRODUCTION
  (always compiled, probed at boot by `probe_nic_at_boot`).
- `set_driver_ok()` + a per-device `SpinLock` are in place as TX/RX scaffolding.
- `FrameAllocator` (cibos_kernel::FrameAllocator) yields 4 KiB `PhysFrame`s; the
  bootloader installs a 0..4 GiB IDENTITY map, so a frame's physical address is
  directly accessible at the same virtual address — exactly what virtqueue DMA
  needs (allocate frame → hand phys addr to device → access at phys==virt).
- `send_frame`/`recv_frame` currently return Busy/None HONESTLY (no fake).

## The legacy split virtqueue (virtio 0.9.5 / legacy PCI) — what to build
A virtqueue has three regions in one physically-contiguous area, sized by the
queue size N the device reports:
  1. Descriptor Table: N × 16-byte descriptors {addr(u64), len(u32), flags(u16),
     next(u16)}.
  2. Available Ring (driver→device): {flags(u16), idx(u16), ring[N](u16),
     used_event(u16)}.
  3. Used Ring (device→driver): {flags(u16), idx(u16), ring[N]{id(u32),len(u32)},
     avail_event(u16)} — must start on a page-aligned boundary after a padding gap.
Layout + alignment per the legacy spec: desc table, then avail ring, then PAD to
the next 4096 boundary, then used ring.

### Legacy virtio-pci queue setup (I/O BAR registers, offsets from io_base)
  0x0E QUEUE_SELECT (w u16): select the queue index.
  0x0C QUEUE_SIZE   (r u16): device-reported N (0 = queue absent).
  0x08 QUEUE_PFN    (rw u32): physical page-frame number (phys addr >> 12) of the
       queue area. Write to attach; the device computes region offsets from N.
  0x10 QUEUE_NOTIFY (w u16): write the queue index to tell the device new buffers
       are available.
  0x12 ISR (r u8): interrupt status (acknowledge on read) — for polled mode we can
       poll the used ring's idx instead.

virtio-net queues: queue 0 = RX (receiveq), queue 1 = TX (transmitq). (queue 2 =
control, optional — not needed for basic TX/RX.)

### virtio-net header
Every TX/RX frame is prefixed by a virtio_net_hdr (legacy: 10 bytes when neither
mergeable-rxbuf nor GSO negotiated): {flags u8, gso_type u8, hdr_len u16,
gso_size u16, csum_start u16, csum_offset u16}. For plain frames: all zero. We did
NOT negotiate MRG_RXBUF/GSO (we only took MAC+STATUS), so the 10-byte header is
correct. RX buffers must reserve these 10 bytes before the Ethernet frame.

## Increments (each real + QEMU-verifiable)
B3a. Virtqueue allocation + setup: allocate page-aligned contiguous frames per
     queue (RX, TX), zero them, write QUEUE_SELECT/QUEUE_PFN, store the ring
     pointers in VirtioNet. Assert QUEUE_SIZE matches our allocation. Call
     set_driver_ok() after both queues are set up.
B3b. RX path: pre-fill the RX ring with buffers (each: 10-byte hdr + 1514 frame),
     publish them in the avail ring, notify. `recv_frame` polls the used ring:
     if used.idx advanced, copy the frame (skip the 10-byte hdr) into the caller
     buffer, recycle the descriptor back into avail. Returns Ok(Some(len)) or
     Ok(None).
B3c. TX path: `send_frame` takes a free TX descriptor, writes [10-byte zero hdr |
     frame] into its buffer, publishes in avail, notifies, then (polled) waits for
     the used ring to reclaim it. Returns Ok(()) or Busy if no descriptor free.
B3d. QEMU verification: with `-netdev user` + `-object filter-dump` OR a second
     guest, send an ARP/ICMP and observe TX in the dump; for RX, QEMU user-net
     replies to DHCP/ARP — observe a received frame. Honest harness note if full
     RX needs the TCP/IP stack (smoltcp, B4) to elicit replies; at minimum prove a
     frame is TX'd (visible in -filter-dump) and the used ring advances.

## Anti-drift / production rules
- Real virtqueue per the virtio spec — works on any virtio-net (cloud, bare-metal
  SR-IOV), not QEMU-specific. QEMU verifies it.
- DMA memory via FrameAllocator + identity map; no hardcoded addresses.
- Polled mode first (no IRQ handler dependency); IRQ-driven RX can come later.
- send_frame/recv_frame keep returning honest errors until each path is real.
- NetDevice trait unchanged → the Lattice transport (B4) binds to the trait, not
  the driver, so apps stay unchanged (NETWORKING.md guarantee).

## After B3: B4 — NIC under the Lattice
Implement a NIC-backed transport behind the SAME Gate/Link/Warden surface: a
smoltcp (no_std) TCP/IP stack port driving the NetDevice; Lattice Links map to
TCP/UDP over the NIC. Loopback stays the default; NIC transport is selected when a
NIC is present. Apps unchanged.

---

## PROGRESS

### Foundation landed (this session)
- `FrameAllocator::allocate_contiguous(n)` — allocates `n` physically-CONTIGUOUS
  frames (the DMA-memory primitive the virtqueues need). Bitmap scan for `n`
  consecutive clear bits; marks the run; honest `LimitExceeded` if no run fits.
  +2 host tests (consecutive-frames, too-large-fails). 355 tests green; bare +
  host build clean. This is the prerequisite for B3a (virtqueue allocation).

### Next (B3a): with allocate_contiguous in hand
- In virtio_net.rs: compute the legacy virtqueue size for N (desc 16N + avail
  6+2N, PAD to 4096, used 6+8N), allocate `ceil(total/4096)` contiguous frames,
  zero them (identity map: phys==virt), store ring pointers, write QUEUE_SELECT
  + QUEUE_PFN for queue 0 (RX) and queue 1 (TX), assert QUEUE_SIZE matches, then
  set_driver_ok(). Then B3b (RX fill+poll), B3c (TX), B3d (QEMU -filter-dump).
