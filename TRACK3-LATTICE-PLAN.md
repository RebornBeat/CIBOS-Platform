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
