# Track 3 — Networking: complete remaining-work capture + sequencing

Bare-metal-FIRST throughout: every driver/stack targets a standardized hardware
or protocol interface; QEMU/cloud/real-hardware merely VERIFY it. No QEMU-first.

## CURRENT STATE (verified)
- NetDevice trait (cibos-kernel/src/net_device.rs): portable, coarse errors,
  in-memory LoopbackNic test impl. The hardware-abstraction seam. DONE.
- virtio-net driver (kernel-image/src/arch/virtio_net.rs): real virtio-pci
  discovery + negotiation + split virtqueues; TX VERIFIED (frame captured in a
  QEMU filter-dump pcap); recv_frame is REAL (polls the used ring, copies out,
  recycles) but NOT yet verified receiving an actual frame. Production-compiled,
  probed at boot. #GP spurious-IRQ fault FIXED (8259 spurious handling).
- Lattice on the kernel (cibos-kernel/src/gate.rs): Gate/Link/Warden over
  syscalls 23-30, byte transport = the canonical Channel (LOOPBACK). DONE +
  QEMU-verified. The doc explicitly says a NIC transport must slot the SAME
  surface beneath these APIs (apps unchanged).
- GAP: the probed NIC is DROPPED after probe (probe_nic_at_boot returns bool).
  Nothing stores it; the Lattice can't use it yet.

## DEPENDENCY-STANCE DECISION (smoltcp) — resolved against the canon
The workspace uses external crates ONLY for (a) cryptography (pqcrypto-* — DIY is
dangerous) and (b) foundational primitives (linked_list_allocator; proc-macro
trio). ALL OS logic — scheduler, IPC, channels, gates, drivers, FS — is
from-scratch. A TCP/IP stack is OS LOGIC, the kind this project writes itself.
DECISION: do NOT pull smoltcp. A TCP/IP stack is exactly the from-scratch systems
work the vision is about, and an external async TCP stack would be the largest
non-from-scratch surface in the kernel (drift). Instead: write a SMALL, honest,
from-scratch L2/L3 layer sized to what the Lattice actually needs (Ethernet +
ARP + IPv4 + UDP first; TCP later as a dedicated effort). This stays aligned and
keeps the NetDevice/Lattice seam clean.
(If TCP later proves too large to do well from scratch, revisit — but start
from-scratch, matching how channels/gates/scheduler were built.)

## REMAINING WORK (ordered; each real + bare-metal-first + QEMU-verified)

### N1 — Verify virtio-net RX (close the TX/RX loop)
recv_frame is written but unproven. Faithful verification WITHOUT inventing a
stack: QEMU user-net answers ARP/ICMP/DHCP. Simplest real proof: the device, on
SoftAP/user-net, will respond to an ARP request we TX. Send an ARP-request for
the gateway (10.0.2.2), then poll recv_frame for the ARP-reply; confirm a frame
is received and its EtherType is ARP (0x0806). This proves RX end-to-end against
the real device. (A loopback NIC alternative is also fine for a unit-level check.)
Anti-drift: real frames only; honest "no frame yet" while polling.

### N2 — Store the NIC (wiring prerequisite)
Keep the probed VirtioNet in a kernel-global (like ROOT_FS / CHANNEL_TABLE) so
later layers can use it: `static NIC: SpinLock<Option<Box<dyn NetDevice>>>` (or a
concrete holder). probe_nic_at_boot installs it. Honest: None when absent.

### N3 — e1000 driver (second NetDevice — non-virtio bare metal)
Real Intel 82540EM (e1000) driver: PCI discovery (vendor 0x8086, device 0x100E),
MMIO BAR mapping, the e1000 descriptor rings (RDBAL/RDBAH/RDLEN/RDH/RDT for RX;
TDBAL/.. for TX), MAC from EEPROM/RAL-RAH, link via STATUS. Implements the SAME
NetDevice trait. Probe order: try virtio-net, then e1000; bind the first present.
QEMU verifies via `-device e1000`. This is why NetDevice exists — apps/Lattice
never change. (e1000 is a real, ubiquitous NIC; not QEMU-specific.)

### N4 — From-scratch L2/L3 net core (cibos-net crate)
A new no_std `cibos-net` crate (workspace-internal, from-scratch):
  - Ethernet framing (parse/build; MAC addressing).
  - ARP (resolve IPv4->MAC; a small cache).
  - IPv4 (header parse/build, checksum, fragmentation deferred honestly).
  - UDP (datagram send/recv) FIRST — enough for DNS/DHCP/datagram Links.
  - ICMP echo (ping) for verifiability.
  - TCP as a SEPARATE later milestone (state machine; the big one).
Driven by a NetDevice; testable on LoopbackNic (host) + QEMU (real device).

### N5 — Wire the NIC under the Lattice (the NETWORKING.md guarantee)
Add a NIC-backed transport selectable beneath the SAME Gate/Link/Warden surface:
  - loopback stays the default (intra-host Links via Channel).
  - when a Link's Gate targets a remote endpoint, route bytes through cibos-net
    (UDP first: Link<->UDP socket; TCP later) over the stored NIC.
  - apps/SDK Lattice calls UNCHANGED (bind/connect/accept/send/recv/warden/probe).
This realizes "only the fabric's backing transport changes" from NETWORKING.md.

### N6 — Per-arch (later, with the arch sweep)
The NIC drivers are x86-specific (PCI port I/O / MMIO). aarch64/riscv64 NICs use
MMIO virtio or platform NICs; captured in the arch sweep, not here. The
NetDevice trait + cibos-net core are arch-independent and carry over.

## SEQUENCING (do in this order)
N1 (RX verify) -> N2 (store NIC) -> N3 (e1000) -> N4 (cibos-net L2/L3, UDP) ->
N5 (NIC under Lattice, UDP Links) -> [TCP milestone] -> N6 (per-arch, in sweep).
Archive after each landed increment. Bare-metal-first; QEMU only verifies.

---

## PROGRESS

### N1 — RX VERIFIED (done)
Replaced the TX-only self-check with a real TX+RX ARP round-trip: the kernel
builds a 42-byte ARP request (our IP 10.0.2.15, target the gateway 10.0.2.2),
sends it via send_frame, then polls recv_frame for the ARP reply (EtherType
0x0806, OPER=2). QEMU output:
    virtio-net TX: ARP request sent
    virtio-net RX: ARP reply — gw 10.0.2.2 is at 52:55:0a:00:02:02
    boot complete
The gateway MAC came from the real device answering through the RX virtqueue —
unfakeable proof RX works end to end. Both TX and RX now proven against the
actual device. 355 tests green; all configs/arches clean. Next: N2 (store NIC).

### N2 — store the NIC (done)
Added the `NIC` kernel-global (kernel-image/src/boot.rs), mirroring `ROOT_FS`:
`SpinLock<Option<Box<dyn NetDevice + Send>>>`. probe_nic_at_boot now boxes the
probed VirtioNet and installs it (after the self-check) instead of dropping it.
Accessors `with_nic(f)` and `nic_present()` are the seam the Lattice's NIC-backed
transport (N5) will use. VirtioNet auto-satisfies Send (fields are usize/atomics/
MacAddress). Verified: boots with the NIC, TX+RX ARP round-trip intact, no fault,
boot complete. 355 tests green; production + demo clean. Next: N3 (e1000 driver).

---

## N3-N5 SWEEP — built + verified in one pass

### N3 — e1000 driver (done)
kernel-image/src/arch/e1000.rs: real Intel 82540EM driver (vendor 0x8086, device
0x100E). PCI discovery, MMIO BAR mapping (identity-mapped), EEPROM MAC read,
legacy RX/TX descriptor rings (RDBAL/RDLEN/RDH/RDT, TDBAL/.., RCTL/TCTL), polled
send/recv. Implements the SAME NetDevice trait. Always-compiled production;
probe_nic_at_boot tries virtio-net first, then e1000, installing the first
present. Shared ARP self-check helper (nic_arp_selfcheck) covers both.

### N4 — cibos-net from-scratch L2/L3 (done)
New no_std workspace crate `cibos-net`, ZERO external crates (forbid(unsafe_code)
too). Modules: ethernet (II framing), arp (parse/build + small cache), ipv4
(header + checksum), udp (datagram + pseudo-header checksum), icmp (echo). The
internet checksum + pseudo-header sum are from scratch. 15 unit tests, clippy
clean, builds host + bare. TCP deferred to a later milestone (honest scope).

### N5 — NIC under the Lattice transport (done)
kernel-image/src/net_stack.rs: ties the stored NIC + cibos-net into a UDP
transport (udp_send_to / poll_udp) with ARP resolution, next-hop (on-link vs
gateway) logic, and answers inbound ARP-for-us + ICMP echo (host is pingable).
configure(mac) sets host IP at boot (static 10.0.2.15; DHCP later). This is the
byte path the Lattice's remote Links bind to — the Gate/Link/Warden surface is
unchanged; only a Link's backing transport can now be the NIC instead of loopback.

### VERIFIED end-to-end (QEMU, real device + real service)
Boot log with a NIC:
    virtio-net TX: ARP request sent
    virtio-net RX: ARP reply — gw 10.0.2.2 is at 52:55:0a:00:02:02
    net-stack UDP: DNS query sent to 10.0.2.3:53
    net-stack UDP: DNS reply from 10.0.2.3:53 (52 bytes) — STACK OK
    boot complete
The DNS round-trip exercises the WHOLE stack: ARP resolve -> Ethernet/IPv4/UDP
build (from-scratch cibos-net) -> NIC TX -> NIC RX -> IPv4+UDP parse -> port
match. 370 tests green (+15 cibos-net); all configs + all 4 arches build clean;
production + demo clean. Bare-metal-first: e1000/virtio-pci/cibos-net target
standard interfaces; QEMU only verifies.

### Remaining in the arc
- Connect the Lattice's connect()/Link path to net_stack for REMOTE gates (UDP
  Links end to end through the kernel APIs) — the final integration step; the
  transport + surface both exist, this is the routing glue + addressing model.
- TCP milestone (state machine) in cibos-net.
- N6 per-arch NICs (in the arch sweep).
