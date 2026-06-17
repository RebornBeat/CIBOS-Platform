//! # Channels
//!
//! Point-to-point message channels between isolation boundaries, the system's
//! IPC primitive.
//!
//! A channel is a bounded queue of byte messages. Sends and receives are
//! asynchronous and integrate with Catch-and-Release:
//!
//! * A receive on an empty channel registers a [`WaitResource::ChannelData`]
//!   wait and parks; a later send wakes it.
//! * A send to a full channel registers a [`WaitResource::ChannelBuffer`] wait
//!   and parks (back-pressure); a later receive wakes it.
//!
//! Messages are *copied* into and out of the channel buffer. Copying is the
//! correct semantics across an isolation boundary: the sender and receiver
//! never share the message's backing memory, so neither can observe or mutate
//! the other's copy.
//!
//! ## Lock discipline
//!
//! Each channel guards its buffer with its own [`SpinLock`]. Every operation
//! computes what scheduler call it needs (wake a waiter, register a wait) while
//! holding the channel lock, then **releases the channel lock before calling the
//! kernel**. The channel lock and the scheduler lock therefore never nest, which
//! rules out a lock-ordering deadlock between them.

use crate::sync::SpinLock;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use shared::protocols::ipc::{ChannelTerms, WaitResource};
use shared::types::error::ProtocolError;
use shared::{ChannelId, KernelInterface, LaneId};

struct ChannelState {
    messages: VecDeque<Vec<u8>>,
    receiver_waiter: Option<LaneId>,
    sender_waiter: Option<LaneId>,
    closed: bool,
}

struct ChannelInner {
    id: ChannelId,
    capacity: usize,
    max_message_bytes: usize,
    kernel: Arc<dyn KernelInterface>,
    state: SpinLock<ChannelState>,
}

/// A cloneable handle to a channel. Both endpoints hold one; cloning shares the
/// same underlying buffer.
#[derive(Clone)]
pub struct Channel {
    inner: Arc<ChannelInner>,
}

/// The synchronous result of attempting a send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendStep {
    /// The message was enqueued.
    Sent,
    /// The buffer is full; the lane has been registered to wait.
    Full,
    /// The channel is closed.
    Closed,
    /// The message exceeds the channel's maximum message size.
    TooLarge,
}

/// The synchronous result of attempting a receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvStep {
    /// A message was dequeued.
    Message(Vec<u8>),
    /// The buffer is empty; the lane has been registered to wait.
    Empty,
    /// The channel is closed and drained.
    Closed,
}

// Actions decided under the channel lock, executed after releasing it.
enum SendAction {
    SentWake(Option<LaneId>),
    RegisterFull,
    Closed,
    TooLarge,
}

enum RecvAction {
    GotWake(Vec<u8>, Option<LaneId>),
    RegisterEmpty,
    Closed,
}

impl Channel {
    /// Create a channel from negotiated `terms`, signalling readiness through
    /// `kernel`.
    #[must_use]
    pub fn new(id: ChannelId, terms: &ChannelTerms, kernel: Arc<dyn KernelInterface>) -> Self {
        Channel {
            inner: Arc::new(ChannelInner {
                id,
                capacity: terms.buffer_capacity as usize,
                max_message_bytes: terms.max_message_bytes as usize,
                kernel,
                state: SpinLock::new(ChannelState {
                    messages: VecDeque::new(),
                    receiver_waiter: None,
                    sender_waiter: None,
                    closed: false,
                }),
            }),
        }
    }

    /// The channel identifier.
    #[must_use]
    pub fn id(&self) -> ChannelId {
        self.inner.id
    }

    /// Number of messages currently buffered.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.inner.state.lock().messages.len()
    }

    /// Attempt to send `msg` on behalf of `lane`, registering a back-pressure
    /// wait if the buffer is full.
    pub fn try_send(&self, lane: LaneId, msg: &[u8]) -> SendStep {
        let action = {
            let mut s = self.inner.state.lock();
            if s.closed {
                SendAction::Closed
            } else if msg.len() > self.inner.max_message_bytes {
                SendAction::TooLarge
            } else if s.messages.len() < self.inner.capacity {
                s.messages.push_back(msg.to_vec());
                SendAction::SentWake(s.receiver_waiter.take())
            } else {
                s.sender_waiter = Some(lane);
                SendAction::RegisterFull
            }
        }; // channel lock released here

        match action {
            SendAction::SentWake(waiter) => {
                if let Some(r) = waiter {
                    self.inner.kernel.signal_ready(r);
                }
                SendStep::Sent
            }
            SendAction::RegisterFull => {
                self.inner
                    .kernel
                    .register_wait(lane, WaitResource::ChannelBuffer(self.inner.id));
                SendStep::Full
            }
            SendAction::Closed => SendStep::Closed,
            SendAction::TooLarge => SendStep::TooLarge,
        }
    }

    /// Attempt to receive on behalf of `lane`, registering a wait if the buffer
    /// is empty.
    pub fn try_recv(&self, lane: LaneId) -> RecvStep {
        let action = {
            let mut s = self.inner.state.lock();
            if let Some(msg) = s.messages.pop_front() {
                RecvAction::GotWake(msg, s.sender_waiter.take())
            } else if s.closed {
                RecvAction::Closed
            } else {
                s.receiver_waiter = Some(lane);
                RecvAction::RegisterEmpty
            }
        }; // channel lock released here

        match action {
            RecvAction::GotWake(msg, waiter) => {
                if let Some(snd) = waiter {
                    self.inner.kernel.signal_ready(snd);
                }
                RecvStep::Message(msg)
            }
            RecvAction::RegisterEmpty => {
                self.inner
                    .kernel
                    .register_wait(lane, WaitResource::ChannelData(self.inner.id));
                RecvStep::Empty
            }
            RecvAction::Closed => RecvStep::Closed,
        }
    }

    /// Close the channel, waking any parked sender and receiver so they observe
    /// the closure.
    pub fn close(&self) {
        let (recv_waiter, send_waiter) = {
            let mut s = self.inner.state.lock();
            s.closed = true;
            (s.receiver_waiter.take(), s.sender_waiter.take())
        };
        if let Some(r) = recv_waiter {
            self.inner.kernel.signal_ready(r);
        }
        if let Some(s) = send_waiter {
            self.inner.kernel.signal_ready(s);
        }
    }

    /// A future that sends `payload` on behalf of `lane`, awaiting buffer space
    /// under back-pressure.
    #[must_use]
    pub fn send(&self, lane: LaneId, payload: Vec<u8>) -> ChannelSend {
        ChannelSend {
            channel: self.clone(),
            lane,
            payload,
        }
    }

    /// A future that receives a message on behalf of `lane`, awaiting data.
    #[must_use]
    pub fn recv(&self, lane: LaneId) -> ChannelRecv {
        ChannelRecv {
            channel: self.clone(),
            lane,
        }
    }
}

/// Future that completes when `payload` has been enqueued (or the channel is
/// closed / the message is too large).
pub struct ChannelSend {
    channel: Channel,
    lane: LaneId,
    payload: Vec<u8>,
}

impl Future for ChannelSend {
    type Output = Result<(), ProtocolError>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this.channel.try_send(this.lane, &this.payload) {
            SendStep::Sent => Poll::Ready(Ok(())),
            SendStep::Full => Poll::Pending,
            SendStep::Closed => Poll::Ready(Err(ProtocolError::ChannelClosed)),
            SendStep::TooLarge => Poll::Ready(Err(ProtocolError::MessageTooLarge {
                size: this.payload.len(),
                maximum: this.channel.inner.max_message_bytes,
            })),
        }
    }
}

/// Future that completes with a received message (or a closed-channel error).
pub struct ChannelRecv {
    channel: Channel,
    lane: LaneId,
}

impl Future for ChannelRecv {
    type Output = Result<Vec<u8>, ProtocolError>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this.channel.try_recv(this.lane) {
            RecvStep::Message(m) => Poll::Ready(Ok(m)),
            RecvStep::Empty => Poll::Pending,
            RecvStep::Closed => Poll::Ready(Err(ProtocolError::ChannelClosed)),
        }
    }
}

/// Assigns channel identifiers and runs the cross-boundary request/accept-or-
/// reject handshake. Channels themselves are reference-counted handles shared
/// between endpoints; the registry just hands out unique ids.
///
/// HANDSHAKE MODE (profile behavioral flag, ADR-007 / roadmap 2.2): this is the
/// canonical request/accept STRUCTURE — terms proposed by the requester, accepted
/// wholesale or rejected, point-to-point. It is the **lightweight-handshake**
/// form (correct for the Compute profile). The Maximum-Isolation / Balanced
/// profiles additionally require **cryptographic-ipc**: the same request/accept
/// structure with the proposal/acceptance authenticated and messages protected.
/// That crypto layer is an ADDITIVE behavioral-flag mode over this same handshake
/// (declared-but-inert today, deferred with the other profile flags) — NOT a
/// different protocol. Labeling it here keeps the future crypto mode aligned with
/// the canonical model rather than reading as drift.
pub struct ChannelRegistry {
    next_id: SpinLock<u64>,
    /// Pending cross-boundary channel requests awaiting the target boundary's
    /// accept-or-reject decision. Keyed by request id. A channel only ever comes
    /// into existence once the TARGET boundary explicitly accepts — a requester
    /// can never force contact into another boundary (binary isolation: the
    /// boundary is the principal, cross-boundary contact is mutual). Terms are
    /// accepted WHOLESALE or rejected; there is no counter-proposal.
    pending: SpinLock<alloc::collections::BTreeMap<u64, PendingRequest>>,
    next_request: SpinLock<u64>,
}

/// A queued cross-boundary channel request: who asked, whom they want to reach,
/// and the exact terms they proposed. Held until the target accepts or rejects.
#[derive(Clone)]
struct PendingRequest {
    /// The boundary that issued the request.
    from: shared::BoundaryId,
    /// The boundary being asked to accept (point-to-point: exactly one target).
    target: shared::BoundaryId,
    /// The terms proposed by the requester (accepted wholesale or rejected).
    terms: ChannelTerms,
}

impl ChannelRegistry {
    /// Create a new registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: SpinLock::new(1),
            pending: SpinLock::new(alloc::collections::BTreeMap::new()),
            next_request: SpinLock::new(1),
        }
    }

    /// Allocate a fresh channel built from `terms`, backed by `kernel`.
    #[must_use]
    pub fn create(&self, terms: &ChannelTerms, kernel: Arc<dyn KernelInterface>) -> Channel {
        let id = {
            let mut n = self.next_id.lock();
            let id = ChannelId::new(*n);
            *n += 1;
            id
        };
        Channel::new(id, terms, kernel)
    }

    /// Queue a cross-boundary channel request from `from` to `request.target`
    /// with the proposed `request.terms`. Returns a request id the requester can
    /// later use to learn the outcome. The channel does NOT exist yet — it only
    /// comes into being if the target boundary accepts.
    #[must_use]
    pub fn request(
        &self,
        from: shared::BoundaryId,
        request: &shared::protocols::ipc::ChannelRequest,
    ) -> u64 {
        let id = {
            let mut n = self.next_request.lock();
            let id = *n;
            *n += 1;
            id
        };
        self.pending.lock().insert(
            id,
            PendingRequest {
                from,
                target: request.target,
                terms: request.terms.clone(),
            },
        );
        id
    }

    /// The next pending request TARGETING `target`, if any. Returns the request
    /// id, the requesting boundary, and the proposed terms so the receiver can
    /// decide. Only requests aimed at `target` are visible — a boundary can never
    /// observe requests meant for another (point-to-point isolation).
    #[must_use]
    pub fn poll(
        &self,
        target: shared::BoundaryId,
    ) -> Option<(u64, shared::BoundaryId, ChannelTerms)> {
        let pending = self.pending.lock();
        pending
            .iter()
            .find(|(_, r)| r.target == target)
            .map(|(id, r)| (*id, r.from, r.terms.clone()))
    }

    /// Accept a pending request WHOLESALE, creating the channel from exactly the
    /// proposed terms. Returns the created `Channel` together with the REQUESTER's
    /// boundary, so the caller can register a handle for BOTH endpoints (both
    /// point at this one Channel — one ChannelId reported to both ends). Returns
    /// `None` if the request id is unknown or if `target` is not the request's
    /// actual target — a boundary can only accept requests aimed at IT.
    #[must_use]
    pub fn accept(
        &self,
        request_id: u64,
        target: shared::BoundaryId,
        kernel: Arc<dyn KernelInterface>,
    ) -> Option<(Channel, shared::BoundaryId)> {
        let req = {
            let mut pending = self.pending.lock();
            match pending.get(&request_id) {
                Some(r) if r.target == target => pending.remove(&request_id),
                _ => None,
            }
        }?;
        // Accept-ALL: the channel is built from the requester's proposed terms,
        // unchanged. The receiver does not get to alter them.
        let channel = self.create(&req.terms, kernel);
        Some((channel, req.from))
    }

    /// Reject a pending request: drop it. The requester learns it was rejected.
    /// Only the actual target may reject. Returns whether a request was removed.
    pub fn reject(&self, request_id: u64, target: shared::BoundaryId) -> bool {
        let mut pending = self.pending.lock();
        match pending.get(&request_id) {
            Some(r) if r.target == target => pending.remove(&request_id).is_some(),
            _ => false,
        }
    }

    /// Whether a request id is still pending (neither accepted nor rejected).
    /// The requester polls this to learn the outcome.
    #[must_use]
    pub fn is_pending(&self, request_id: u64) -> bool {
        self.pending.lock().contains_key(&request_id)
    }
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::protocols::ipc::ChannelDirection;
    use shared::types::time::Monotonic;
    use std::sync::Mutex;

    // A tiny kernel stub recording signals, for unit-testing channel mechanics
    // in isolation from the scheduler.
    struct StubKernel {
        signals: Mutex<Vec<LaneId>>,
        waits: Mutex<Vec<(LaneId, WaitResource)>>,
    }
    impl StubKernel {
        fn new() -> Arc<Self> {
            Arc::new(StubKernel {
                signals: Mutex::new(Vec::new()),
                waits: Mutex::new(Vec::new()),
            })
        }
    }
    impl KernelInterface for StubKernel {
        fn register_wait(&self, lane: LaneId, resource: WaitResource) {
            self.waits.lock().unwrap().push((lane, resource));
        }
        fn signal_ready(&self, lane: LaneId) {
            self.signals.lock().unwrap().push(lane);
        }
        fn now(&self) -> Monotonic {
            Monotonic::ZERO
        }
    }

    fn terms(cap: u32, max_msg: u32) -> ChannelTerms {
        ChannelTerms::new("test", ChannelDirection::Bidirectional, max_msg, cap).unwrap()
    }

    #[test]
    fn send_then_receive() {
        let k = StubKernel::new();
        let ch = Channel::new(ChannelId::new(1), &terms(4, 64), k.clone());
        assert_eq!(ch.try_send(LaneId::new(1), b"hello"), SendStep::Sent);
        assert_eq!(ch.pending(), 1);
        assert_eq!(
            ch.try_recv(LaneId::new(2)),
            RecvStep::Message(b"hello".to_vec())
        );
        assert_eq!(ch.pending(), 0);
    }

    #[test]
    fn receive_empty_registers_wait() {
        let k = StubKernel::new();
        let ch = Channel::new(ChannelId::new(7), &terms(4, 64), k.clone());
        assert_eq!(ch.try_recv(LaneId::new(2)), RecvStep::Empty);
        let waits = k.waits.lock().unwrap();
        assert_eq!(waits.len(), 1);
        assert_eq!(waits[0], (LaneId::new(2), WaitResource::ChannelData(ChannelId::new(7))));
    }

    #[test]
    fn send_wakes_waiting_receiver() {
        let k = StubKernel::new();
        let ch = Channel::new(ChannelId::new(1), &terms(4, 64), k.clone());
        // Receiver parks.
        assert_eq!(ch.try_recv(LaneId::new(20)), RecvStep::Empty);
        // Sender sends; the parked receiver is signalled.
        assert_eq!(ch.try_send(LaneId::new(10), b"data"), SendStep::Sent);
        assert_eq!(*k.signals.lock().unwrap(), alloc::vec![LaneId::new(20)]);
    }

    #[test]
    fn full_buffer_applies_back_pressure() {
        let k = StubKernel::new();
        let ch = Channel::new(ChannelId::new(1), &terms(2, 64), k.clone());
        assert_eq!(ch.try_send(LaneId::new(1), b"a"), SendStep::Sent);
        assert_eq!(ch.try_send(LaneId::new(1), b"b"), SendStep::Sent);
        // Third send hits the capacity ceiling and registers a buffer wait.
        assert_eq!(ch.try_send(LaneId::new(1), b"c"), SendStep::Full);
        let waits = k.waits.lock().unwrap();
        assert_eq!(
            waits[0],
            (LaneId::new(1), WaitResource::ChannelBuffer(ChannelId::new(1)))
        );
    }

    #[test]
    fn receive_wakes_waiting_sender() {
        let k = StubKernel::new();
        let ch = Channel::new(ChannelId::new(1), &terms(1, 64), k.clone());
        assert_eq!(ch.try_send(LaneId::new(1), b"a"), SendStep::Sent);
        assert_eq!(ch.try_send(LaneId::new(1), b"b"), SendStep::Full); // parks sender
        // A receive frees a slot and wakes the parked sender.
        assert_eq!(ch.try_recv(LaneId::new(2)), RecvStep::Message(b"a".to_vec()));
        assert_eq!(*k.signals.lock().unwrap(), alloc::vec![LaneId::new(1)]);
    }

    #[test]
    fn oversized_message_rejected() {
        let k = StubKernel::new();
        let ch = Channel::new(ChannelId::new(1), &terms(4, 4), k.clone());
        assert_eq!(ch.try_send(LaneId::new(1), b"toolong"), SendStep::TooLarge);
    }

    #[test]
    fn closed_channel_reports_closed() {
        let k = StubKernel::new();
        let ch = Channel::new(ChannelId::new(1), &terms(4, 64), k.clone());
        ch.close();
        assert_eq!(ch.try_send(LaneId::new(1), b"x"), SendStep::Closed);
        assert_eq!(ch.try_recv(LaneId::new(2)), RecvStep::Closed);
    }

    // ---- Cross-boundary handshake (request / poll / accept / reject) ----------

    use shared::protocols::ipc::ChannelRequest;
    use shared::BoundaryId;

    fn channel_request(target: u64, cap: u32, max_msg: u32) -> ChannelRequest {
        ChannelRequest {
            target: BoundaryId(target),
            terms: terms(cap, max_msg),
        }
    }

    #[test]
    fn handshake_accept_creates_channel_from_proposed_terms() {
        let k = StubKernel::new();
        let reg = ChannelRegistry::new();
        let from = BoundaryId(0x100);
        let req = channel_request(0x200, 4, 64);

        let id = reg.request(from, &req);
        assert!(reg.is_pending(id), "request should be pending until decided");

        // The target boundary (0x200) sees the pending request and its terms.
        let polled = reg.poll(BoundaryId(0x200)).expect("target should see request");
        assert_eq!(polled.0, id);
        assert_eq!(polled.1, from, "requester boundary reported to receiver");
        assert_eq!(polled.2, req.terms, "exact proposed terms reported");

        // Accept WHOLESALE -> a channel exists, same id usable by both ends, and
        // the requester's boundary is reported so both endpoints can be wired.
        let (channel, requester) = reg
            .accept(id, BoundaryId(0x200), k.clone())
            .expect("accept should create the channel");
        assert!(!reg.is_pending(id), "accepted request no longer pending");
        assert_eq!(channel.id(), ChannelId::new(1));
        assert_eq!(requester, from, "requester boundary returned for endpoint wiring");
    }

    #[test]
    fn handshake_reject_drops_request_no_channel() {
        let k = StubKernel::new();
        let reg = ChannelRegistry::new();
        let id = reg.request(BoundaryId(0x100), &channel_request(0x200, 4, 64));

        assert!(reg.reject(id, BoundaryId(0x200)), "target may reject");
        assert!(!reg.is_pending(id), "rejected request is gone");
        // A rejected request cannot then be accepted.
        assert!(reg.accept(id, BoundaryId(0x200), k).is_none());
    }

    #[test]
    fn handshake_is_point_to_point_wrong_boundary_cannot_see_or_accept() {
        let k = StubKernel::new();
        let reg = ChannelRegistry::new();
        let id = reg.request(BoundaryId(0x100), &channel_request(0x200, 4, 64));

        // A boundary that is NOT the target cannot observe the request...
        assert!(reg.poll(BoundaryId(0x999)).is_none(), "non-target sees nothing");
        // ...nor accept it (a requester cannot force contact into another boundary,
        // and a third boundary cannot hijack the request).
        assert!(reg.accept(id, BoundaryId(0x999), k.clone()).is_none());
        assert!(!reg.reject(id, BoundaryId(0x999)));
        // The legitimate target still can.
        assert!(reg.is_pending(id));
        assert!(reg.accept(id, BoundaryId(0x200), k).is_some());
    }

    #[test]
    fn handshake_poll_only_returns_requests_for_that_target() {
        let reg = ChannelRegistry::new();
        let to_a = reg.request(BoundaryId(0x1), &channel_request(0xA, 4, 64));
        let _to_b = reg.request(BoundaryId(0x2), &channel_request(0xB, 8, 32));

        let pa = reg.poll(BoundaryId(0xA)).expect("A's request visible to A");
        assert_eq!(pa.0, to_a);
        // Boundary A only sees the request aimed at it, not the one aimed at B.
        assert_eq!(pa.2.buffer_capacity, 4);
    }
}
