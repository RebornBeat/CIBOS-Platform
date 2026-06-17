# Track 3 — Lattice networking on the kernel (Gate/Link/Warden over syscalls)

## TRUE STATUS (reviewed, not assumed)
- The **Lattice MODEL + API** is implemented in `cibos-sdk/src/net.rs` as an
  in-memory **loopback fabric**: Gate (port, u16), Link (bidirectional byte
  stream), Listener, Warden (per-Gate allow/deny firewall), Probe (range scan).
- NETWORKING.md is explicit and authoritative: the loopback fabric is "the genuine
  networking MODEL and API"; a NIC driver + packet transport is a SEPARATE
  hardware-dependent layer that "will implement the same Gate/Link/Warden surface
  BENEATH these APIs. Applications written against the Lattice will not change
  when that layer is added; only the fabric's backing transport does."
- The KERNEL has NO net syscalls yet — on-kernel ring-3 apps cannot use the
  Lattice. (Channels went SDK -> kernel earlier; the Lattice has not.)

## FAITHFUL TRACK-3 STEP (no drift, no fakes, QEMU-verifiable)
NOT a hardware NIC (that is the spec's explicitly-deferred hardware layer; it
cannot be runtime-verified without real hardware and would violate the no-fakes
discipline). Instead: bring the **Lattice surface onto the kernel over syscalls**,
backed by the LOOPBACK transport — exactly the surface a future NIC slots beneath.
This mirrors how the channel handshake went from SDK to kernel and is fully
QEMU-verifiable.

A `Link` is architecturally a `Channel` + a Gate (port) address + a Warden
(policy) check. So the kernel Lattice REUSES the canonical kernel `Channel` for
the byte stream (no second IPC system), adding:
  - a Gate registry: Gate(u16) -> bound boundary + a pending-connect queue;
  - the Warden: per-Gate allow/deny AND boundary ownership (the spec: "binding
    ownership to boundaries lets the Warden answer which boundary may use this
    Gate"). A denied Gate is neither bindable nor connectable — denial is TOTAL.

### Syscalls (numbers 23+), mirroring the channel-handshake pattern
  23 GateBind(gate)            -> listener handle | Blocked(warden) | InUse
       caller boundary (from trap) becomes the Gate's owner.
  24 GateConnect(gate)         -> link handle | Blocked | Refused(no listener)
       creates a canonical Channel pair; both ends get a handle (like accept).
  25 GateAccept(listener)      -> link handle | WouldBlock(no pending)
       the bound owner accepts a pending connect -> the other half of the Channel.
  26 LinkSend(handle, bytes)   -> 0 | WouldBlock | Closed   (canonical try_send)
  27 LinkRecv(handle, buf)     -> n  | WouldBlock | Closed   (canonical try_recv)
  28 LinkClose(handle)         -> 0
  29 WardenSet(gate, allow)    -> 0   (per-Gate policy; owner/SYSTEM only)
  30 GateProbe(gate)           -> open | closed | blocked   (Probe one Gate)

### Anti-drift invariants (must hold)
  - REUSE the canonical kernel `Channel` for the byte stream (no parallel IPC).
  - Gate/Link/Warden surface IDENTICAL in shape to the SDK Lattice (the NIC layer
    must be able to slot beneath the SAME surface) — names + semantics match
    NETWORKING.md.
  - The Warden is boundary-aware: a Gate is owned by the boundary that bound it;
    denial is TOTAL (bind AND connect refused). Caller boundary from the trap,
    never user-supplied.
  - Loopback only this increment; the NIC transport is honestly deferred (the
    spec's hardware layer). Flag it, do not fake it.
  - Binary boundary isolation preserved: a Link crosses boundaries only via a
    Gate the Warden permits; bytes pass THROUGH the kernel.
  - Brief locks only; one selector (the existing shared Scheduler) for wakeups.

## PLAN (smallest faithful increments, each tested)
A. Kernel-side `GateRegistry` (in cibos-kernel, host-tested): Gate -> {owner
   boundary, pending connects}; Warden allow/deny + ownership; bind/connect/accept
   producing canonical Channels. Host unit tests: bind, connect->accept handshake,
   Warden denial total, boundary ownership, probe states.
B. Syscalls 23-30: enum + dispatch + SyscallEnv trait defaults + KernelSyscallEnv
   impls (reusing the channel handle table for Links). Dispatch tests.
C. Ring-3 SDK wrappers (cibos-app) + an on-kernel QEMU demo: boundary X binds a
   Gate, boundary Y connects, X accepts, bytes flow X<->Y over the Link; a
   Warden-denied Gate refuses both bind and connect.
D. Build-all (default/interactive/multilane/arches) + clippy + full test suite +
   QEMU verify + archive.

## Honest scope
Proves the Lattice Gate/Link/Warden surface ON THE KERNEL over loopback — the
stable model the spec says a NIC will later back. The actual NIC driver + packet
transport stays deferred (hardware layer), flagged not faked.

---

## PROGRESS

### Increment A — DONE (kernel-side GateRegistry, host-tested)
`cibos-kernel/src/gate.rs` (332 lines) implements the kernel Lattice:
GateRegistry with bind/unbind/connect/accept/probe + a boundary-aware Warden
(deny is TOTAL — blocks bind AND connect). A Link is ONE canonical `Channel`
(both halves share its Arc inner) — REUSES the channel system, no parallel IPC.
connect queues the listener's half; the owner accept()s it (ownership enforced).
Mirrors the SDK Lattice surface (Gate/Link/Warden/Probe) so a future NIC transport
slots beneath the SAME surface.
Resolved the open `ChannelId` visibility question: ChannelId lives in `shared`
(pub), so gate.rs uses `shared::ChannelId` (matching channel.rs). 7 host tests
green: bind/probe, double-bind, warden-total-denial, connect-unbound-refused,
connect→accept-one-channel, accept-ownership. Full suite 344/0.

### Next — Increment B (net syscalls 23-30)
GateBind/GateConnect/GateAccept/LinkSend/LinkRecv/LinkClose/WardenSet/GateProbe,
mirroring the channel-handshake syscall pattern (enum + from_number + dispatch +
SyscallEnv trait defaults + KernelSyscallEnv impls reusing the channel handle
table for Link byte streams). Caller boundary from the trap. Then C (ring-3 SDK
wrappers + QEMU demo), then D (build-all/clippy/tests/QEMU/archive).

### Increment B — DONE (net syscalls 23-30, host + dispatch tested)
shared: 8 Syscall variants (GateBind/GateConnect/GateAccept/LinkSend/LinkRecv/
LinkClose/WardenSet/GateProbe) + from_number arms; gate passed as u16 scalar,
Link bytes by pointer (copy_from/to_user). cibos-kernel: 8 SyscallEnv trait
methods (default NotPermitted) + 8 dispatch arms (gate range-checked; caller
boundary from the trap, never user-supplied). KernelSyscallEnv impls back them
with the table's GateRegistry; Links are canonical Channels registered in the
SAME handle table (reuse, no parallel system); a fresh ChannelId per Link.
GateError -> SyscallError mapping: Blocked/AlreadyBound/NotOwner -> NotPermitted,
Refused -> NotFound, WouldBlock -> WouldBlock. Tests: 7 gate registry + 3
dispatch (reaches-env, gate-range, link-close). Full suite 347/0, clippy clean,
all configs/arches build clean.

### Next — Increment C (ring-3 SDK wrappers + QEMU demo)
cibos-app wrappers: gate_bind/gate_connect/gate_accept/link_send/link_recv/
link_close/warden_set/gate_probe (thin syscall wrappers like the channel ones).
An on-kernel demo: boundary X binds a Gate, Y connects, X accepts, bytes flow
X<->Y over the Link; a Warden-denied Gate refuses both bind and connect. Then D
(build-all/clippy/tests/QEMU/archive).

### Increment C — DONE (ring-3 SDK wrappers + QEMU demo), Increment D — DONE (verified+archived)
cibos-app/src/net.rs: ring-3 Lattice wrappers — bind→Listener, connect→Link,
Listener::accept, Link::send/recv/close, warden_set, probe → GateState. Matches
the SDK Lattice vocabulary. kernel-image/ring3.rs: demonstrate_lattice drives the
REAL KernelSyscallEnv net methods with S=0x300 (binds) / C=0x400 (connects).

RUNTIME-VERIFIED in QEMU (one boot, alongside the cross-boundary handshake demo):
  Warden-denied gate 81: bind REFUSED + connect REFUSED (total denial)
  probe gate 80: Closed -> (S binds) -> Open
  C connected -> client link; S accepted -> server link (two halves of ONE Channel)
  C sent 'GET /' -> S received 'GET /' (LATTICE LINK OK)
  S replied '200 OK' -> C received '200 OK' (BIDIRECTIONAL OK)
347 tests / 0 failing; default + interactive + multilane + aarch64 + riscv64 +
cibos-app all build clean; clippy clean.

## TRACK 3A STATUS: COMPLETE
The Lattice (Gate/Link/Warden/Probe) is real ON THE KERNEL over syscalls 23-30,
backed by the canonical Channel (loopback transport), runtime-verified. This is
the stable surface a NIC transport will sit beneath.

## NEXT: Track 3B — NetDevice trait + virtio-net driver
A real `NetDevice` trait (mirroring the BlockDevice pattern) + a virtio-net driver
behind it (real interface, QEMU-verifiable), then smoltcp TCP/IP under the Lattice
so the loopback transport can be swapped for NIC-backed connectivity WITHOUT
changing the Gate/Link/Warden surface or any app (NETWORKING.md guarantee).

---

## TRACK 3B — NetDevice trait + virtio-net driver (reviewed design)

### Status of 3A (done, verified)
The kernel Lattice (Gate/Link/Warden over syscalls 23-30) is built and QEMU-
verified end to end (bind/connect/accept producing one canonical Channel; total
Warden denial; bidirectional bytes through the kernel). 347 tests green.

### 3B goal (faithful, no fakes)
A real NIC layer BENEATH the Lattice, following the VERIFIED BlockDevice/ATA
pattern exactly:
  - `cibos-kernel/src/net_device.rs`: a portable `NetDevice` trait (the storage
    analogue is `block.rs`). Packet-oriented (NICs are frame devices, not block
    arrays): `mac() -> [u8;6]`, `link_up() -> bool`, `send_frame(&[u8]) ->
    Result<(), NetError>`, `recv_frame(&mut [u8]) -> Result<Option<usize>,
    NetError>` (None = no packet waiting), `mtu()`. Coarse `NetDeviceError`
    (LinkDown/TooLarge/DeviceError/Busy) mapped by the driver from hardware
    status — mirrors `BlockError`. An in-memory loopback `NetDevice` for tests
    (the analogue of the test RamDisk).
  - `kernel-image/src/arch/virtio_net.rs`: a concrete `virtio-net` driver
    (virtio-net IS a real, ubiquitous interface — a real driver, not a fake;
    QEMU exposes it, as do cloud hypervisors and bare-metal SR-IOV). virtio-net
    over MMIO (the legacy/modern virtio transport), with virtqueues (RX/TX rings)
    — the same role ATA-PIO plays for storage: the first real driver, with
    PCI/MMIO enumeration. Serialized by a SpinLock like AtaDisk.

### Anti-drift invariants
  - The Lattice Gate/Link/Warden SURFACE does NOT change. 3B adds a transport
    beneath it; apps and the net syscalls are untouched (NETWORKING.md guarantee:
    "applications will not change when that layer is added; only the fabric's
    backing transport does").
  - NetDevice is portable (in cibos-kernel); the driver is arch/hardware (in
    kernel-image/arch) — exactly the block.rs vs ata.rs split.
  - Honest hardware boundary: a real virtio-net driver is QEMU-verifiable (QEMU
    provides the device). Frame TX/RX is provable; a full TCP/IP stack on top
    (smoltcp-port) is the SEPARATE next layer, flagged not faked.
  - No second IPC/Channel system; this is a device layer, orthogonal to Channels.

### Increments (each tested, QEMU where hardware is involved)
B1. `NetDevice` trait + `NetDeviceError` + an in-memory loopback impl + host unit
    tests (send/recv/link-down/too-large/mtu). (cibos-kernel — no hardware.)
B2. virtio-net driver skeleton: PCI/MMIO probe + virtqueue setup + mac read +
    link status. Detects the QEMU virtio-net-device. (kernel-image/arch.)
B3. Frame TX/RX over the virtqueues; a boot-time demo that brings the device up,
    reads its MAC, sends a frame and receives one (e.g. an ARP or a loopback via
    QEMU's socket backend) — QEMU-verified.
B4. Wire NetDevice beneath the Lattice as an alternate transport (loopback stays
    the default; NIC transport selectable) WITHOUT changing the Gate/Link/Warden
    surface. Then (separate) smoltcp for TCP/IP.
B5. build-all + clippy + tests + QEMU + archive.

---

## PROGRESS — 3B increments B1 + B2 DONE (verified)

### B1 — NetDevice trait (cibos-kernel/src/net_device.rs) — DONE
Portable frame-oriented trait mirroring BlockDevice: mac()/link_up()/mtu()/
send_frame()/recv_frame(), coarse NetDeviceError (LinkDown/TooLarge/Busy/
DeviceError), + an in-memory loopback NIC for tests. 6 host tests green
(mac+mtu, send/recv round-trip, recv-none, oversized rejected, small-buffer
too-large, link-down blocks both). Builds host + bare. Net suite: 353/0.

### B2 — virtio-net driver discovery + negotiation (kernel-image/src/arch/virtio_net.rs) — DONE
Real legacy virtio-pci driver: PCI bus enumeration over config space
(0xCF8/0xCFC), virtio-net detection (vendor 0x1AF4 / device 0x1000), I/O-BAR
discovery, the legacy device-init handshake (reset→ACK→DRIVER→feature
negotiation→FEATURES_OK), MAC read from device config, link status via
VIRTIO_NET_F_STATUS. Implements NetDevice (send/recv return Busy/None honestly
until the rings land — no fake success). Gated behind `virtio-net-demo` until the
Lattice transport wires it (B4).
QEMU-VERIFIED (real proof): booting `-device virtio-net-pci,mac=52:54:00:ab:cd:ef`
printed `virtio-net found — MAC 52:54:00:ab:cd:ef` (the EXACT QEMU-assigned MAC,
read from the real device config — unfakeable) and `link is UP`. Without a device:
honest `no virtio-net device on the PCI bus (skipping)`. All configs/arches +
clippy clean.

### Honest remaining in 3B
- B3: frame TX/RX over the virtqueues (RX/TX rings, DMA frames). The largest part;
  set_driver_ok + the lock are the scaffolding already in place for it.
- B4: wire NetDevice beneath the Lattice as an alternate transport (loopback stays
  default), WITHOUT changing the Gate/Link/Warden surface; then smoltcp for TCP/IP.

---

## PRODUCTION-CORRECTNESS REVIEW (user concern: "building for QEMU, not bare metal")

Audited the virtio-net driver against the concern that we might be adapting to
QEMU rather than building for production bare metal verified VIA QEMU.

### Finding 1 — the driver itself is NOT QEMU-specific (good)
Every register access targets the virtio-pci SPECIFICATION, not QEMU:
  - PCI config space via 0xCF8/0xCFC = the standard x86 PCI mechanism (every real
    PC/server), not a QEMU detail.
  - vendor 0x1AF4 / device 0x1000 = the virtio STANDARD's assigned IDs.
  - reset→ACK→DRIVER→features→FEATURES_OK = the virtio spec's mandated init
    sequence, verbatim.
  - MAC/status read = the standard virtio-net config layout.
A real machine presenting virtio-net (every cloud VM; bare-metal SR-IOV) runs this
exact code. QEMU is the TEST HARNESS — the same role it plays for the ATA driver
(which also runs on real ATA/SATA disks). This is the BlockDevice/ATA precedent.

### Finding 2 — DRIFT FROM THE PRODUCTION PATTERN (must fix): demo-gating
ATA is `pub mod ata` — ALWAYS compiled, production code, probed at boot
(`mount_root_fs_early` → `AtaDisk::probe`), with no demo gate. The virtio-net
driver was wrongly gated behind the `virtio-net-demo` FEATURE, so it is NOT in
production builds — exactly the "built for the demo, not for production" smell.
FIX (match the ATA precedent): make `virtio_net` an always-compiled production
module; probe it at boot like ATA; let `virtio-net-demo` control only the verbose
probe LOGGING, never whether the driver exists. (Below.)

### Finding 3 — single-NIC coverage (real, the trait already anticipates it)
virtio-net is paravirtual (VMs/cloud); a bare-metal box may have Intel e1000/igb
or Realtek instead. The `NetDevice` trait exists precisely so multiple concrete
drivers sit behind it. Faithful path: virtio-net now (verifiable), e1000 next
(also a real standardized interface), with a boot probe that tries each and binds
the first present. This is real hardware coverage, not QEMU adaptation.

### Standing principle (recorded so we don't drift again)
Drivers are PRODUCTION CODE compiled into the kernel and probed at boot. QEMU (or
any hypervisor, or real hardware) is where we VERIFY them — we never build "for
QEMU." A device-specific demo feature may add logging, never gate the driver's
existence. Identifying real hardware at boot (probing multiple drivers) may take
longer but is the correct production behavior.

### RESOLUTION (done + verified)
- `virtio_net` is now `pub mod virtio_net` — ALWAYS compiled on x86_64, no demo
  gate (matches `ata`). 
- `probe_nic_at_boot()` runs UNCONDITIONALLY at boot (production path), like the
  ATA storage probe. It tries virtio-net (e1000 next) and honestly reports the
  result. The `virtio-net-demo` feature now only adds a verbose negotiation line.
- VERIFIED in QEMU with the PRODUCTION image (NO demo feature):
    * NIC attached  → "NIC: virtio-net MAC 52:54:00:ab:cd:ef, link up" (real MAC
      read from the device, matching the QEMU-assigned value — unfakeable proof).
    * NIC absent    → "NIC: no supported NIC found (loopback only)" (honest
      fallback, no crash, no fake).
- 353 tests green; default + virtio-net-demo + interactive + multilane + aarch64
  + riscv64 all build clean; clippy clean.
- Principle locked in: drivers are production code probed at boot; QEMU/cloud/
  bare-metal merely VERIFY them. No more demo-gating of real drivers.

### Remaining for Track 3B (honest)
- e1000 driver (second NetDevice impl) so bare-metal boxes without virtio work.
- virtio-net TX/RX over the virtqueues (B3) — the `set_driver_ok`/ring scaffolding
  is in place and marked.
- Wire the chosen NIC under the Lattice's NIC-backed transport (B4), beneath the
  SAME Gate/Link/Warden surface (apps unchanged — NETWORKING.md guarantee).
