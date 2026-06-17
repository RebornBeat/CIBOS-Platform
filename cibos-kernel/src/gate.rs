//! The Lattice on the kernel: Gate / Link / Warden over the canonical channel.
//!
//! This is the kernel-side backing of the networking model documented in
//! `NETWORKING.md`. The vocabulary and semantics match the SDK Lattice exactly,
//! because a future NIC + packet transport must be able to slot the SAME
//! Gate/Link/Warden surface beneath these APIs without applications changing.
//!
//! * A **Gate** (`u16`, the "port") is BOUND by a boundary, which becomes its
//!   owner and receives a listener. Another boundary CONNECTS to a Gate to reach
//!   the bound service.
//! * A **Link** is an established bidirectional byte stream. It is backed by the
//!   canonical [`Channel`](crate::channel::Channel) — the SAME kernel IPC the
//!   cross-boundary channel handshake uses — so bytes pass THROUGH the kernel,
//!   never via shared user memory.
//! * The **Warden** is the firewall: per-Gate allow/deny, checked on BOTH bind
//!   and connect. Denial is TOTAL — a denied Gate is neither bindable nor
//!   reachable. The Warden is boundary-aware: a Gate is owned by the boundary
//!   that bound it.
//!
//! This increment is LOOPBACK only (one CIBOS instance). The off-machine NIC
//! transport is the spec's separate hardware-dependent layer, honestly deferred.

extern crate alloc;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::Arc;

use shared::protocols::ipc::{ChannelDirection, ChannelTerms, KernelInterface};
use shared::BoundaryId;

use crate::channel::Channel;
use crate::sync::SpinLock;

/// A Gate identifier — the "port". Matches the SDK `Gate = u16`.
pub type Gate = u16;

/// The result of probing a single Gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateState {
    /// A listener is bound and the Warden allows the Gate.
    Open,
    /// No listener is bound (but the Warden allows it).
    Closed,
    /// The Warden denies the Gate.
    Blocked,
}

/// Why a Lattice operation failed. Mirrors the SDK `NetError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateError {
    /// No listener is bound on the target Gate.
    Refused,
    /// The Gate is already bound by another listener.
    AlreadyBound,
    /// The Warden denies this Gate (bind and connect both refused).
    Blocked,
    /// No pending connect is waiting to be accepted.
    WouldBlock,
    /// The caller does not own this Gate (e.g. tried to accept/set policy on a
    /// Gate it did not bind).
    NotOwner,
}

/// A pending connect awaiting the Gate owner's `accept`: the connecting boundary
/// and the listener's half of the freshly-created Link channel.
struct PendingConnect {
    /// The boundary that issued the connect.
    from: BoundaryId,
    /// The listener-side channel handle (the owner gets this on accept). The
    /// connector already holds the peer handle (the same `Channel`).
    listener_link: Channel,
}

/// One bound Gate: its owner and the queue of connects awaiting accept.
struct BoundGate {
    owner: BoundaryId,
    pending: VecDeque<PendingConnect>,
}

/// The kernel Lattice: the Gate registry + Warden. Backs bind/connect/accept and
/// per-Gate policy. Links are canonical channels; this owns only the Gate
/// addressing + firewall, not a second byte-transport.
pub struct GateRegistry {
    /// Bound gates, keyed by Gate number. A Gate not present here is unbound.
    bound: SpinLock<BTreeMap<Gate, BoundGate>>,
    /// Warden deny list: gates explicitly denied. Default-allow (empty = allow
    /// all), matching the SDK Warden's "allows all" default.
    denied: SpinLock<BTreeSet<Gate>>,
    /// Terms for Link channels (bidirectional byte stream, generous bounds).
    /// Fixed for loopback; a NIC transport would map these to MTU/window later.
    link_capacity: u32,
    link_max_message: u32,
}

impl GateRegistry {
    /// Create an empty Lattice (no gates bound, Warden allows all).
    #[must_use]
    pub fn new() -> Self {
        Self {
            bound: SpinLock::new(BTreeMap::new()),
            denied: SpinLock::new(BTreeSet::new()),
            link_capacity: 16,
            link_max_message: 2048,
        }
    }

    /// Whether the Warden currently allows `gate` (default-allow).
    #[must_use]
    pub fn is_allowed(&self, gate: Gate) -> bool {
        !self.denied.lock().contains(&gate)
    }

    /// Warden: deny a Gate (bind AND connect refused). Total denial.
    pub fn warden_deny(&self, gate: Gate) {
        self.denied.lock().insert(gate);
    }

    /// Warden: allow a previously-denied Gate.
    pub fn warden_allow(&self, gate: Gate) {
        self.denied.lock().remove(&gate);
    }

    /// Bind `gate` for `owner`. Returns Ok on success; the caller now owns the
    /// Gate and may `accept` connects on it. `Blocked` if the Warden denies it,
    /// `AlreadyBound` if another listener holds it.
    pub fn bind(&self, owner: BoundaryId, gate: Gate) -> Result<(), GateError> {
        if !self.is_allowed(gate) {
            return Err(GateError::Blocked);
        }
        let mut bound = self.bound.lock();
        if bound.contains_key(&gate) {
            return Err(GateError::AlreadyBound);
        }
        bound.insert(
            gate,
            BoundGate {
                owner,
                pending: VecDeque::new(),
            },
        );
        Ok(())
    }

    /// Release a Gate the caller owns (e.g. listener closed).
    pub fn unbind(&self, owner: BoundaryId, gate: Gate) -> Result<(), GateError> {
        let mut bound = self.bound.lock();
        match bound.get(&gate) {
            Some(g) if g.owner == owner => {
                bound.remove(&gate);
                Ok(())
            }
            Some(_) => Err(GateError::NotOwner),
            None => Err(GateError::Refused),
        }
    }

    /// Connect to `gate` from `from`. Creates one canonical Link channel; the
    /// CONNECTOR's half is returned now, the LISTENER's half is queued for the
    /// owner to `accept`. `Blocked` if the Warden denies the Gate; `Refused` if
    /// no listener is bound.
    pub fn connect(
        &self,
        from: BoundaryId,
        gate: Gate,
        kernel: Arc<dyn KernelInterface>,
        channel_id: shared::ChannelId,
    ) -> Result<Channel, GateError> {
        if !self.is_allowed(gate) {
            return Err(GateError::Blocked);
        }
        let mut bound = self.bound.lock();
        let g = bound.get_mut(&gate).ok_or(GateError::Refused)?;

        // One Link = one canonical Channel; both endpoints share its Arc inner,
        // so a clone is the peer handle to the SAME byte stream.
        let terms = ChannelTerms::new(
            "lattice-link",
            ChannelDirection::Bidirectional,
            self.link_max_message,
            self.link_capacity,
        )
        .map_err(|_| GateError::Refused)?;
        let link = Channel::new(channel_id, &terms, kernel);

        g.pending.push_back(PendingConnect {
            from,
            listener_link: link.clone(),
        });
        Ok(link)
    }

    /// Accept the next pending connect on `gate` (the caller must own it).
    /// Returns the listener's half of the Link plus the connecting boundary.
    /// `WouldBlock` if no connect is pending; `NotOwner`/`Refused` otherwise.
    pub fn accept(
        &self,
        owner: BoundaryId,
        gate: Gate,
    ) -> Result<(Channel, BoundaryId), GateError> {
        let mut bound = self.bound.lock();
        let g = bound.get_mut(&gate).ok_or(GateError::Refused)?;
        if g.owner != owner {
            return Err(GateError::NotOwner);
        }
        let pc = g.pending.pop_front().ok_or(GateError::WouldBlock)?;
        Ok((pc.listener_link, pc.from))
    }

    /// Probe a single Gate: Open (bound + allowed), Closed (allowed, unbound), or
    /// Blocked (Warden denies). Mirrors the SDK `Probe`.
    #[must_use]
    pub fn probe(&self, gate: Gate) -> GateState {
        if !self.is_allowed(gate) {
            return GateState::Blocked;
        }
        if self.bound.lock().contains_key(&gate) {
            GateState::Open
        } else {
            GateState::Closed
        }
    }
}

impl Default for GateRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::ChannelId;
    use shared::protocols::ipc::WaitResource;
    use shared::LaneId;
    use shared::types::time::Monotonic;

    /// A no-op KernelInterface for tests (Links never park in these unit tests).
    struct StubKernel;
    impl KernelInterface for StubKernel {
        fn register_wait(&self, _lane: LaneId, _resource: WaitResource) {}
        fn signal_ready(&self, _lane: LaneId) {}
        fn now(&self) -> Monotonic {
            Monotonic::ZERO
        }
        fn set_lane_weight(&self, _lane: LaneId, _weight: u32) -> bool {
            false
        }
    }

    fn kernel() -> Arc<dyn KernelInterface> {
        Arc::new(StubKernel)
    }

    #[test]
    fn bind_then_probe_open() {
        let reg = GateRegistry::new();
        assert_eq!(reg.probe(80), GateState::Closed);
        reg.bind(BoundaryId(0x100), 80).unwrap();
        assert_eq!(reg.probe(80), GateState::Open);
    }

    #[test]
    fn double_bind_is_already_bound() {
        let reg = GateRegistry::new();
        reg.bind(BoundaryId(0x100), 80).unwrap();
        assert_eq!(reg.bind(BoundaryId(0x200), 80), Err(GateError::AlreadyBound));
    }

    #[test]
    fn warden_denial_is_total() {
        let reg = GateRegistry::new();
        reg.warden_deny(80);
        assert_eq!(reg.probe(80), GateState::Blocked);
        // Bind refused.
        assert_eq!(reg.bind(BoundaryId(0x100), 80), Err(GateError::Blocked));
        // Connect refused (even though nothing is bound, the Warden check is first).
        assert_eq!(
            reg.connect(BoundaryId(0x200), 80, kernel(), ChannelId::new(1)).err(),
            Some(GateError::Blocked)
        );
        // Re-allow and it binds.
        reg.warden_allow(80);
        assert!(reg.bind(BoundaryId(0x100), 80).is_ok());
    }

    #[test]
    fn connect_to_unbound_is_refused() {
        let reg = GateRegistry::new();
        assert_eq!(
            reg.connect(BoundaryId(0x200), 80, kernel(), ChannelId::new(1)).err(),
            Some(GateError::Refused)
        );
    }

    #[test]
    fn connect_then_accept_links_both_ends_to_one_channel() {
        let reg = GateRegistry::new();
        let owner = BoundaryId(0x100);
        let client = BoundaryId(0x200);
        reg.bind(owner, 80).unwrap();

        // Client connects -> gets its half.
        let client_link = reg.connect(client, 80, kernel(), ChannelId::new(1)).unwrap();

        // Owner accepts -> gets the listener half + the client's boundary.
        let (listener_link, from) = reg.accept(owner, 80).unwrap();
        assert_eq!(from, client);
        assert_eq!(listener_link.id(), client_link.id(), "same Link channel");

        // Bytes flow client -> owner through the SAME channel.
        use crate::channel::{RecvStep, SendStep};
        assert_eq!(client_link.try_send(LaneId::new(1), b"ping"), SendStep::Sent);
        match listener_link.try_recv(LaneId::new(2)) {
            RecvStep::Message(m) => assert_eq!(m.as_slice(), b"ping"),
            other => panic!("expected ping, got {other:?}"),
        }
    }

    #[test]
    fn accept_requires_ownership_and_pending() {
        let reg = GateRegistry::new();
        let owner = BoundaryId(0x100);
        reg.bind(owner, 80).unwrap();
        // No pending connect yet.
        assert_eq!(reg.accept(owner, 80).err(), Some(GateError::WouldBlock));
        // A non-owner cannot accept.
        reg.connect(BoundaryId(0x200), 80, kernel(), ChannelId::new(1)).unwrap();
        assert_eq!(reg.accept(BoundaryId(0x999), 80).err(), Some(GateError::NotOwner));
        // The owner can.
        assert!(reg.accept(owner, 80).is_ok());
    }
}
