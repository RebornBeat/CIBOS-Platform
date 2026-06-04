//! # Lattice — the CIBOS network fabric
//!
//! The Lattice is CIBOS's networking layer. It is deliberately *not* a Unix
//! sockets clone; it uses CIBOS-native concepts:
//!
//! * A **Gate** is a numbered endpoint (the "port"). A component *binds* a Gate
//!   to offer a service and gets a [`Listener`]; another component *connects* to
//!   a Gate to reach that service.
//! * A **Link** is an established bidirectional byte-stream connection between
//!   two endpoints (the "socket").
//! * The **Warden** is the firewall: per-Gate allow/deny rules, checked on both
//!   bind and connect. Isolation-aware by design — a denied Gate is neither
//!   bindable nor reachable.
//!
//! This implementation is an **in-memory loopback fabric**: Links are backed by
//! shared message queues, so all communication stays within one CIBOS instance.
//! That is exactly what is testable without hardware, and it presents the same
//! Gate/Link/Warden API a NIC-backed transport will implement later — apps that
//! use the Lattice do not change when real networking is added beneath it.

use cibos_kernel::Channel;
use shared::LaneId;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};

/// A Gate number — the CIBOS equivalent of a port.
pub type Gate = u16;

/// Networking errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// No listener is bound on the target Gate.
    Refused,
    /// The Gate is already bound by another listener.
    AlreadyBound,
    /// The Warden denies this Gate.
    Blocked,
    /// The Link has been closed by the peer.
    LinkClosed,
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::Refused => write!(f, "connection refused"),
            NetError::AlreadyBound => write!(f, "gate already bound"),
            NetError::Blocked => write!(f, "blocked by warden"),
            NetError::LinkClosed => write!(f, "link closed"),
        }
    }
}

impl std::error::Error for NetError {}

/// One direction's message queue.
struct Queue {
    messages: VecDeque<Vec<u8>>,
    closed: bool,
}

impl Queue {
    fn new() -> Arc<Mutex<Queue>> {
        Arc::new(Mutex::new(Queue {
            messages: VecDeque::new(),
            closed: false,
        }))
    }
}

/// An established bidirectional connection. Each endpoint sends on its `tx`
/// queue and receives on its `rx` queue; the two endpoints' queues are crossed.
pub struct Link {
    tx: Arc<Mutex<Queue>>,
    rx: Arc<Mutex<Queue>>,
}

impl Link {
    /// Send a message to the peer.
    ///
    /// # Errors
    /// [`NetError::LinkClosed`] if the peer has closed the link.
    pub fn send(&self, data: &[u8]) -> Result<(), NetError> {
        let mut q = self.tx.lock().unwrap();
        if q.closed {
            return Err(NetError::LinkClosed);
        }
        q.messages.push_back(data.to_vec());
        Ok(())
    }

    /// Try to receive a message: `Ok(Some(_))` if one is waiting, `Ok(None)` if
    /// none is waiting yet, `Err(LinkClosed)` if the peer closed and drained.
    pub fn try_recv(&self) -> Result<Option<Vec<u8>>, NetError> {
        let mut q = self.rx.lock().unwrap();
        if let Some(m) = q.messages.pop_front() {
            Ok(Some(m))
        } else if q.closed {
            Err(NetError::LinkClosed)
        } else {
            Ok(None)
        }
    }

    /// Close this link from both directions.
    pub fn close(&self) {
        self.tx.lock().unwrap().closed = true;
        self.rx.lock().unwrap().closed = true;
    }
}

struct ListenerState {
    pending: VecDeque<Link>,
    /// Optional notification channel: rung on connect to wake a parked async
    /// server (see [`Lattice::connect_signaling`] and Vane's live daemon).
    doorbell: Option<Channel>,
}

struct LatticeState {
    listeners: BTreeMap<Gate, ListenerState>,
    denied: BTreeSet<Gate>,
}

/// The shared network fabric. Cloning shares the same fabric.
#[derive(Clone)]
pub struct Lattice {
    inner: Arc<Mutex<LatticeState>>,
}

impl Default for Lattice {
    fn default() -> Self {
        Self::new()
    }
}

impl Lattice {
    /// Create an empty fabric (no listeners, Warden allows all).
    #[must_use]
    pub fn new() -> Self {
        Lattice {
            inner: Arc::new(Mutex::new(LatticeState {
                listeners: BTreeMap::new(),
                denied: BTreeSet::new(),
            })),
        }
    }

    /// Warden: deny a Gate (bind and connect both refused).
    pub fn warden_deny(&self, gate: Gate) {
        self.inner.lock().unwrap().denied.insert(gate);
    }

    /// Warden: allow a previously-denied Gate.
    pub fn warden_allow(&self, gate: Gate) {
        self.inner.lock().unwrap().denied.remove(&gate);
    }

    /// Whether the Warden currently allows a Gate.
    #[must_use]
    pub fn is_allowed(&self, gate: Gate) -> bool {
        !self.inner.lock().unwrap().denied.contains(&gate)
    }

    /// Bind a Gate to offer a service.
    ///
    /// # Errors
    /// [`NetError::Blocked`] if the Warden denies the Gate;
    /// [`NetError::AlreadyBound`] if a listener already holds it.
    pub fn bind(&self, gate: Gate) -> Result<Listener, NetError> {
        let mut s = self.inner.lock().unwrap();
        if s.denied.contains(&gate) {
            return Err(NetError::Blocked);
        }
        if s.listeners.contains_key(&gate) {
            return Err(NetError::AlreadyBound);
        }
        s.listeners.insert(
            gate,
            ListenerState {
                pending: VecDeque::new(),
                doorbell: None,
            },
        );
        Ok(Listener {
            gate,
            lattice: self.clone(),
        })
    }

    /// Connect to a Gate, establishing a Link.
    ///
    /// # Errors
    /// [`NetError::Blocked`] if the Warden denies the Gate;
    /// [`NetError::Refused`] if no listener is bound there.
    pub fn connect(&self, gate: Gate) -> Result<Link, NetError> {
        let mut s = self.inner.lock().unwrap();
        if s.denied.contains(&gate) {
            return Err(NetError::Blocked);
        }
        let listener = s.listeners.get_mut(&gate).ok_or(NetError::Refused)?;
        let c2s = Queue::new();
        let s2c = Queue::new();
        // Server receives on c2s, sends on s2c; client is mirrored.
        let server_link = Link {
            tx: s2c.clone(),
            rx: c2s.clone(),
        };
        let client_link = Link { tx: c2s, rx: s2c };
        listener.pending.push_back(server_link);
        Ok(client_link)
    }

    /// Attach a notification channel ("doorbell") to a bound Gate. A live async
    /// server parks on this channel's `recv`; [`connect_signaling`] rings it so
    /// the server wakes only when a connection actually arrives — no polling.
    ///
    /// [`connect_signaling`]: Lattice::connect_signaling
    pub fn install_doorbell(&self, gate: Gate, doorbell: Channel) {
        if let Some(l) = self.inner.lock().unwrap().listeners.get_mut(&gate) {
            l.doorbell = Some(doorbell);
        }
    }

    /// Like [`connect`](Self::connect), but also rings the Gate's doorbell (if
    /// one is installed) on behalf of `lane`, waking a parked async server. The
    /// notification is best-effort: if the doorbell is full the connection is
    /// still established and will be picked up on the next wake.
    ///
    /// # Errors
    /// Same as [`connect`](Self::connect): [`NetError::Blocked`] or
    /// [`NetError::Refused`].
    pub fn connect_signaling(&self, lane: LaneId, gate: Gate) -> Result<Link, NetError> {
        let link = self.connect(gate)?;
        let doorbell = {
            let s = self.inner.lock().unwrap();
            s.listeners.get(&gate).and_then(|l| l.doorbell.clone())
        };
        if let Some(db) = doorbell {
            let _ = db.try_send(lane, &[1u8]);
        }
        Ok(link)
    }

    /// Scan a set of Gates, reporting each one's status.
    #[must_use]
    pub fn scan(&self, gates: impl IntoIterator<Item = Gate>) -> Vec<GateStatus> {
        let s = self.inner.lock().unwrap();
        gates
            .into_iter()
            .map(|gate| {
                let blocked = s.denied.contains(&gate);
                GateStatus {
                    gate,
                    blocked,
                    open: !blocked && s.listeners.contains_key(&gate),
                }
            })
            .collect()
    }

    /// All Gates with a bound listener.
    #[must_use]
    pub fn open_gates(&self) -> Vec<Gate> {
        self.inner.lock().unwrap().listeners.keys().copied().collect()
    }

    fn accept(&self, gate: Gate) -> Option<Link> {
        self.inner
            .lock()
            .unwrap()
            .listeners
            .get_mut(&gate)
            .and_then(|l| l.pending.pop_front())
    }

    fn unbind(&self, gate: Gate) {
        self.inner.lock().unwrap().listeners.remove(&gate);
    }
}

/// A bound Gate awaiting connections.
pub struct Listener {
    gate: Gate,
    lattice: Lattice,
}

impl Listener {
    /// The Gate this listener holds.
    #[must_use]
    pub fn gate(&self) -> Gate {
        self.gate
    }

    /// Accept the next pending connection, or `None` if none is waiting yet.
    #[must_use]
    pub fn accept(&self) -> Option<Link> {
        self.lattice.accept(self.gate)
    }

    /// Unbind the Gate, refusing further connections.
    pub fn close(self) {
        self.lattice.unbind(self.gate);
    }
}

/// The result of scanning one Gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateStatus {
    /// The Gate number.
    pub gate: Gate,
    /// Whether a listener is bound (and the Gate is allowed).
    pub open: bool,
    /// Whether the Warden denies this Gate.
    pub blocked: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_to_unbound_gate_is_refused() {
        let net = Lattice::new();
        assert_eq!(net.connect(80).err(), Some(NetError::Refused));
    }

    #[test]
    fn bind_connect_exchange() {
        let net = Lattice::new();
        let listener = net.bind(80).unwrap();

        // Client connects and sends a request.
        let client = net.connect(80).unwrap();
        client.send(b"GET /").unwrap();

        // Server accepts and reads it, then replies.
        let server = listener.accept().expect("a pending connection");
        assert_eq!(server.try_recv().unwrap().as_deref(), Some(&b"GET /"[..]));
        server.send(b"200 OK").unwrap();

        // Client reads the reply.
        assert_eq!(client.try_recv().unwrap().as_deref(), Some(&b"200 OK"[..]));
        // Nothing more pending.
        assert_eq!(client.try_recv().unwrap(), None);
    }

    #[test]
    fn double_bind_is_rejected() {
        let net = Lattice::new();
        let _l = net.bind(80).unwrap();
        assert_eq!(net.bind(80).err(), Some(NetError::AlreadyBound));
    }

    #[test]
    fn warden_blocks_bind_and_connect() {
        let net = Lattice::new();
        net.warden_deny(23); // telnet-style gate, denied
        assert_eq!(net.bind(23).err(), Some(NetError::Blocked));
        assert_eq!(net.connect(23).err(), Some(NetError::Blocked));
        // Allowing it again lets a bind through.
        net.warden_allow(23);
        assert!(net.bind(23).is_ok());
    }

    #[test]
    fn close_propagates_to_peer() {
        let net = Lattice::new();
        let listener = net.bind(9).unwrap();
        let client = net.connect(9).unwrap();
        let server = listener.accept().unwrap();
        client.close();
        // Server sees the closed link once drained.
        assert_eq!(server.try_recv(), Err(NetError::LinkClosed));
    }

    #[test]
    fn scan_reports_open_closed_blocked() {
        let net = Lattice::new();
        let _a = net.bind(80).unwrap();
        let _b = net.bind(443).unwrap();
        net.warden_deny(23);

        let report = net.scan([22u16, 23, 80, 443]);
        let by_gate = |g: Gate| report.iter().find(|s| s.gate == g).copied().unwrap();
        assert!(!by_gate(22).open && !by_gate(22).blocked); // closed
        assert!(by_gate(23).blocked); // firewalled
        assert!(by_gate(80).open);
        assert!(by_gate(443).open);

        assert_eq!(net.open_gates(), vec![80, 443]);
    }
}
