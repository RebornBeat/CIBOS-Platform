# CIBOS networking: the Lattice

CIBOS networking is built on its own vocabulary, not the Unix one, because the
model is isolation-first.

| CIBOS term | Conventional equivalent | Status |
|---|---|---|
| **Lattice** | network stack / fabric | implemented (loopback) |
| **Gate** | port | implemented |
| **Link** | socket / connection | implemented |
| **Warden** | firewall | implemented |
| **Probe** | port scanner | implemented (app) |
| **Vane** | server daemon (the "nginx") | planned |
| **Lens** | browser / fetch client | planned |

## Model

* A **Gate** is a numbered endpoint (`u16`). A service *binds* a Gate and
  receives a `Listener`; a client *connects* to a Gate to reach it.
* A **Link** is a bidirectional byte-stream connection — message-framed
  `send`/`try_recv`, with close propagation to the peer.
* The **Warden** is the firewall: per-Gate allow/deny, checked on both *bind*
  and *connect*. A denied Gate cannot be bound or reached — denial is total, in
  keeping with CIBOS's binary-isolation stance (no partial access).
* **Probe** scans a Gate range and reports each as open, closed, or blocked.

Accessed through the SDK: `system.lattice()` returns the shared `Lattice`, the
same way `system.filesystem()` returns the shared filesystem. All tasks of a
system share one fabric.

## Transport and honesty

The current Lattice **surface** is backed by an **in-memory loopback fabric**:
Links are backed by shared message queues, so all traffic stays inside one CIBOS
instance. This is the genuine networking *model* and *API*. The hardware layer
beneath it now EXISTS: two real NIC drivers (virtio-net, e1000) behind a portable
`NetDevice` trait, a from-scratch `cibos-net` stack (Ethernet/ARP/IPv4/UDP/ICMP),
and a `net_stack` UDP transport — verified end to end against a real device and a
real service (an ARP round-trip and a DNS round-trip in QEMU). The probed NIC is
stored in a kernel-global.

The one remaining step is to ROUTE a Link's bytes over that NIC transport for a
REMOTE Gate (instead of the loopback Channel) — the Gate/Link/Warden surface and
all app/SDK calls stay identical; only a Link's backing transport widens. Until
that routing glue lands, the Lattice is loopback-backed and the NIC transport is
exercised directly (`net_stack::udp_send_to` / `poll_udp`). Applications written
against the Lattice will not change when remote Links are wired; only the fabric's
backing transport does.

This mirrors how a real OS is layered: the socket/port/firewall model is stable;
the driver underneath is swappable.

## Isolation and "accounts"

CIBOS has no traditional per-user process accounts. The **isolation boundary**
is the security principal: a boundary owns its lanes, channels, memory, and
(soon) its Gates. The Warden enforces network access policy; binding ownership
to boundaries lets the Warden answer "which boundary may use this Gate" without
a separate account system. Human authentication (password or wired key device,
defined in `shared::types::authentication`) gates entry to a profile; it is
orthogonal to the boundary isolation that contains running code.

## Roadmap

1. **Vane** — a request-serving daemon. Binds a Gate, accepts Links, serves
   content (from the filesystem) over a small request/response protocol.
2. **Lens** — a client that connects to a Vane Gate, issues requests, and
   renders responses. The "browser".
3. A named request protocol over Links (the HTTP equivalent).
4. Gate ownership by boundary, so the Warden can express per-boundary policy.
5. A NIC-backed transport beneath the Lattice for real connectivity.
