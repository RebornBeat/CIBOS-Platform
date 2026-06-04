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

The current Lattice is an **in-memory loopback fabric**: Links are backed by
shared message queues, so all traffic stays inside one CIBOS instance. This is
what is testable without hardware, and it is the genuine networking *model* and
*API*. Real off-machine connectivity is a separate, hardware-dependent layer —
a NIC driver plus a packet transport — that will implement the same
Gate/Link/Warden surface beneath these APIs. Applications written against the
Lattice will not change when that layer is added; only the fabric's backing
transport does.

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
