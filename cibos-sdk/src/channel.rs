//! The Channel API (API Reference, Chapter 2).
//!
//! [`Channel<T>`] is a typed message channel. [`Channel::new_local`] creates an
//! intra-container pair: both ends share one buffer and either end may send or
//! receive. Local channels carry `T` by value with no serialization — the typed
//! payloads live in a shared queue, and a kernel channel of one-byte tokens
//! supplies the Catch-and-Release back-pressure and wake-ups (so a full buffer
//! stalls the sender and an empty buffer stalls the receiver, with no
//! busy-waiting). The token and its payload move in lock-step: on the single
//! cooperative executor nothing runs between reserving a token slot and pushing
//! the payload, so a receiver that observes a token always finds its message.
//!
//! Cross-container channels connect two containers: a requester calls
//! [`Channel::request`] with a target [`ContainerId`](crate::ContainerId) and a
//! lane in the target calls [`await_channel_request`] and accepts. On this host
//! both containers share one executor, so a connected channel is the same typed
//! queue plus token signal as a local one; the request/accept handshake is
//! routed through a shared broker. Each side counts the channel against its own
//! container — outbound for the requester, inbound for the accepter.

use crate::broker::PendingRequest;
use crate::container::ContainerId;
use crate::context::{current_lane, current_system};
use crate::system::System;
use cibos_kernel::{Channel as KernelChannel, RecvStep, SendStep};
use shared::protocols::ipc::{ChannelDirection, ChannelTerms};
use shared::types::error::ProtocolError;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

/// Which container's accounting a channel endpoint belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelRole {
    /// An intra-container channel (`new_local`).
    Local,
    /// The requesting side of a cross-container channel.
    Outbound,
    /// The accepting side of a cross-container channel.
    Inbound,
}

/// Per-side lifecycle guard: holds the owning system and the channel's role, and
/// releases that side's channel count exactly once — when this side's last
/// handle drops (via `Drop`) or when the channel is explicitly closed. Because
/// each side has its own guard, a cross-container channel is counted
/// independently on each container.
struct SideGuard {
    system: System,
    role: ChannelRole,
    released: Cell<bool>,
}

impl SideGuard {
    fn release(&self) {
        if !self.released.replace(true) {
            match self.role {
                ChannelRole::Local => self.system.note_local_channel_closed(),
                ChannelRole::Outbound => self.system.note_outbound_channel_closed(),
                ChannelRole::Inbound => self.system.note_inbound_channel_closed(),
            }
        }
    }
}

impl Drop for SideGuard {
    fn drop(&mut self) {
        self.release();
    }
}

/// The largest buffer a local channel may request.
const MAX_LOCAL_BUFFER: usize = 65536;

/// Errors from channel operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelError {
    /// The target container does not exist.
    TargetNotFound,
    /// The target container rejected the request.
    TargetRejected,
    /// The target did not accept within the timeout.
    TargetTimeout,
    /// The proposed terms exceed system policy.
    TermsViolation,
    /// This container is not authorized to channel the target.
    Unauthorized,
    /// The channel has been closed.
    ChannelClosed,
    /// A message exceeded the channel's maximum message size.
    MessageTooLarge,
    /// The requested buffer capacity was outside the valid range.
    BufferCapacityInvalid,
    /// The container is shutting down.
    ContainerExiting,
}

/// Error from [`Channel::try_send`]; the message is returned to the caller.
#[derive(Debug)]
pub enum TrySendError<T> {
    /// The buffer is full. The message is returned.
    Full(T),
    /// The channel is closed. The message is returned.
    Closed(T),
}

/// Error from [`Channel::try_receive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryReceiveError {
    /// No message is currently available.
    Empty,
    /// The channel is closed and drained.
    Closed,
}

/// State shared by every handle of one local channel.
struct LocalShared<T> {
    queue: RefCell<VecDeque<T>>,
    closed: Cell<bool>,
}

/// A typed message channel. Clone freely; all clones share one buffer.
pub struct Channel<T> {
    shared: Rc<LocalShared<T>>,
    signal: KernelChannel,
    capacity: usize,
    side: Rc<SideGuard>,
}

impl<T> Clone for Channel<T> {
    fn clone(&self) -> Self {
        Channel {
            shared: self.shared.clone(),
            signal: self.signal.clone(),
            capacity: self.capacity,
            side: self.side.clone(),
        }
    }
}

impl<T> Channel<T> {
    /// Assemble an endpoint over shared connection state with a given role.
    fn endpoint(
        shared: Rc<LocalShared<T>>,
        signal: KernelChannel,
        capacity: usize,
        system: System,
        role: ChannelRole,
    ) -> Channel<T> {
        Channel {
            shared,
            signal,
            capacity,
            side: Rc::new(SideGuard {
                system,
                role,
                released: Cell::new(false),
            }),
        }
    }

    /// Create a local (intra-container) channel.
    ///
    /// Returns a pair of ends, conventionally named `(sender, receiver)`; either
    /// end may both send and receive, and both share one buffer of
    /// `buffer_capacity` messages.
    ///
    /// # Errors
    ///
    /// [`ChannelError::BufferCapacityInvalid`] if `buffer_capacity` is `0` or
    /// greater than `65536`; [`ChannelError::TermsViolation`] if the container is
    /// already at its `max_channels` ceiling or the kernel rejects a buffer that
    /// large.
    ///
    /// # Panics
    ///
    /// Panics if called outside a running application (no ambient system).
    pub fn new_local(buffer_capacity: usize) -> Result<(Channel<T>, Channel<T>), ChannelError> {
        if buffer_capacity == 0 || buffer_capacity > MAX_LOCAL_BUFFER {
            return Err(ChannelError::BufferCapacityInvalid);
        }
        let system = current_system();
        // Enforce the container's channel ceiling against the live count of
        // application-visible channels (internal plumbing is not counted).
        if system.local_channel_count() >= system.resource_limits().max_channels {
            return Err(ChannelError::TermsViolation);
        }
        let terms = ChannelTerms::new(
            "local",
            ChannelDirection::Bidirectional,
            1,
            buffer_capacity as u32,
        )
        .map_err(|_| ChannelError::TermsViolation)?;
        let signal = system.open_channel(&terms);
        let shared = Rc::new(LocalShared {
            queue: RefCell::new(VecDeque::new()),
            closed: Cell::new(false),
        });
        // Count this as one application-visible local channel (regardless of how
        // many handles are cloned from it); the side guard releases it once.
        system.note_local_channel_open();
        let sender = Channel::endpoint(shared, signal, buffer_capacity, system, ChannelRole::Local);
        let receiver = sender.clone();
        Ok((sender, receiver))
    }

    /// Request a channel to another container. Awaits the target accepting (or
    /// rejecting); on success both sides hold a connected [`Channel<T>`].
    ///
    /// # Errors
    ///
    /// [`ChannelError::BufferCapacityInvalid`] for a bad capacity;
    /// [`ChannelError::TermsViolation`] if at the channel ceiling or the kernel
    /// rejects the buffer; [`ChannelError::TargetRejected`] if the target
    /// rejects; [`ChannelError::TargetNotFound`] if the target never accepts and
    /// the rendezvous is torn down.
    ///
    /// # Panics
    ///
    /// Panics if called outside a running application (no ambient system).
    pub async fn request(request: ChannelRequest) -> Result<Channel<T>, ChannelError>
    where
        T: 'static,
    {
        if request.buffer_capacity == 0 || request.buffer_capacity > MAX_LOCAL_BUFFER {
            return Err(ChannelError::BufferCapacityInvalid);
        }
        let system = current_system();
        if system.outbound_channel_count() >= system.resource_limits().max_channels {
            return Err(ChannelError::TermsViolation);
        }

        // Shared connection state plus the data token signal.
        let data_terms = ChannelTerms::new(
            request.purpose,
            ChannelDirection::Bidirectional,
            1,
            request.buffer_capacity as u32,
        )
        .map_err(|_| ChannelError::TermsViolation)?;
        let signal = system.open_channel(&data_terms);
        let shared = Rc::new(LocalShared {
            queue: RefCell::new(VecDeque::new()),
            closed: Cell::new(false),
        });

        // Rendezvous signal the requester awaits for accept/reject.
        let accept_terms =
            ChannelTerms::new("xc-accept", ChannelDirection::Bidirectional, 1, 1)
                .map_err(|_| ChannelError::TermsViolation)?;
        let accept_signal = system.open_channel(&accept_terms);

        // The requester's (outbound) endpoint.
        system.note_outbound_channel_open();
        let endpoint = Channel::endpoint(
            shared.clone(),
            signal.clone(),
            request.buffer_capacity,
            system.clone(),
            ChannelRole::Outbound,
        );

        // Hand the target type-erased connection state and the rendezvous.
        let payload: Box<dyn std::any::Any> = Box::new((shared, signal, request.buffer_capacity));
        system.broker().submit(
            request.target,
            PendingRequest {
                source: system.boundary(),
                purpose: request.purpose,
                payload,
                accept_signal: accept_signal.clone(),
            },
            current_lane(),
        );

        // Await the target's decision. The outbound endpoint's side guard
        // releases the count if we drop it on the error paths.
        match accept_signal.recv(current_lane()).await {
            Ok(reply) if reply.first() == Some(&1u8) => Ok(endpoint),
            Ok(_) => Err(ChannelError::TargetRejected),
            Err(_) => Err(ChannelError::TargetNotFound),
        }
    }

    /// The buffer capacity this channel was created with.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Send a message. Stalls (back-pressure) if the buffer is full.
    ///
    /// # Errors
    ///
    /// [`ChannelError::ChannelClosed`] if the channel has been closed.
    pub async fn send(&self, message: T) -> Result<(), ChannelError> {
        // Reserve a buffer slot first (this is the back-pressure point), then
        // enqueue the typed payload. No await separates the two, so a receiver
        // never observes a token without its message.
        match self.signal.send(current_lane(), vec![1u8]).await {
            Ok(()) => {
                self.shared.queue.borrow_mut().push_back(message);
                Ok(())
            }
            Err(ProtocolError::MessageTooLarge { .. }) => Err(ChannelError::MessageTooLarge),
            Err(_) => Err(ChannelError::ChannelClosed),
        }
    }

    /// Receive a message. Stalls if the buffer is empty. Returns `None` once the
    /// channel is closed and drained.
    pub async fn receive(&self) -> Option<T> {
        match self.signal.recv(current_lane()).await {
            Ok(_token) => self.shared.queue.borrow_mut().pop_front(),
            Err(_) => None,
        }
    }

    /// Non-blocking send. Returns the message on failure.
    ///
    /// # Errors
    ///
    /// [`TrySendError::Full`] if the buffer is full; [`TrySendError::Closed`] if
    /// the channel is closed.
    pub fn try_send(&self, message: T) -> Result<(), TrySendError<T>> {
        match self.signal.try_send(current_lane(), &[1u8]) {
            SendStep::Sent => {
                self.shared.queue.borrow_mut().push_back(message);
                Ok(())
            }
            SendStep::Full => Err(TrySendError::Full(message)),
            SendStep::Closed | SendStep::TooLarge => Err(TrySendError::Closed(message)),
        }
    }

    /// Non-blocking receive.
    ///
    /// # Errors
    ///
    /// [`TryReceiveError::Empty`] if no message is available;
    /// [`TryReceiveError::Closed`] if the channel is closed and drained.
    pub fn try_receive(&self) -> Result<T, TryReceiveError> {
        match self.signal.try_recv(current_lane()) {
            RecvStep::Message(_token) => self
                .shared
                .queue
                .borrow_mut()
                .pop_front()
                .ok_or(TryReceiveError::Empty),
            RecvStep::Empty => Err(TryReceiveError::Empty),
            RecvStep::Closed => Err(TryReceiveError::Closed),
        }
    }

    /// Close the channel, consuming this handle. Buffered messages remain
    /// available to receivers; once drained, `receive` returns `None`.
    pub fn close(self) {
        self.shared.closed.set(true);
        self.signal.close();
        self.side.release();
    }
}

/// A request to open a channel to another container.
#[derive(Debug, Clone)]
pub struct ChannelRequest {
    /// The target container.
    pub target: ContainerId,
    /// An informational purpose label for the channel.
    pub purpose: &'static str,
    /// The buffer capacity to negotiate (`1..=65536`).
    pub buffer_capacity: usize,
}

/// An incoming cross-container channel request, delivered to the target by
/// [`await_channel_request`]. Accept it to obtain the connected endpoint, or
/// reject it.
pub struct IncomingRequest<T> {
    source: ContainerId,
    purpose: &'static str,
    shared: Rc<LocalShared<T>>,
    signal: KernelChannel,
    capacity: usize,
    accept_signal: KernelChannel,
    decided: bool,
}

impl<T> IncomingRequest<T> {
    /// The container that made the request.
    #[must_use]
    pub fn source(&self) -> ContainerId {
        self.source
    }

    /// The request's informational purpose label.
    #[must_use]
    pub fn purpose(&self) -> &'static str {
        self.purpose
    }

    /// Accept the request, returning this side's connected (inbound) endpoint and
    /// waking the requester.
    pub fn accept(mut self) -> Channel<T> {
        let system = current_system();
        system.note_inbound_channel_open();
        let endpoint = Channel::endpoint(
            self.shared.clone(),
            self.signal.clone(),
            self.capacity,
            system,
            ChannelRole::Inbound,
        );
        // Wake the requester with an accept marker.
        let _ = self.accept_signal.try_send(current_lane(), &[1u8]);
        self.decided = true;
        endpoint
    }

    /// Reject the request, waking the requester with a rejection.
    pub fn reject(mut self) {
        let _ = self.accept_signal.try_send(current_lane(), &[0u8]);
        self.decided = true;
    }
}

impl<T> Drop for IncomingRequest<T> {
    fn drop(&mut self) {
        // A dropped-without-decision request is treated as a rejection so the
        // requester does not wait forever.
        if !self.decided {
            let _ = self.accept_signal.try_send(current_lane(), &[0u8]);
        }
    }
}

/// Wait for an incoming cross-container channel request addressed to this
/// container, returning it for the caller to accept or reject.
///
/// # Errors
///
/// [`ChannelError::ChannelClosed`] if the container is shutting down and the
/// rendezvous is gone, or if the delivered request's message type does not match
/// `T`.
///
/// # Panics
///
/// Panics if called outside a lane task (no ambient system or lane).
pub async fn await_channel_request<T: 'static>() -> Result<IncomingRequest<T>, ChannelError> {
    let system = current_system();
    let boundary = system.boundary();
    let broker = system.broker();

    loop {
        if let Some(pending) = broker.take(boundary) {
            // Downcast the type-erased connection state to this T.
            let typed = pending
                .payload
                .downcast::<(Rc<LocalShared<T>>, KernelChannel, usize)>()
                .map_err(|_| ChannelError::TermsViolation)?;
            let (shared, signal, capacity) = *typed;
            return Ok(IncomingRequest {
                source: pending.source,
                purpose: pending.purpose,
                shared,
                signal,
                capacity,
                accept_signal: pending.accept_signal,
                decided: false,
            });
        }

        // Mailbox empty: wait on this container's doorbell, creating it on first
        // use. On the cooperative host the empty check and this wait are atomic
        // with respect to other tasks, so a concurrently-submitted request is
        // either taken above or rings the doorbell we are about to wait on.
        let doorbell = broker.doorbell(boundary, || {
            let terms = ChannelTerms::new("xc-doorbell", ChannelDirection::Bidirectional, 1, 64)
                .expect("doorbell terms are valid");
            system.open_channel(&terms)
        });
        if doorbell.recv(current_lane()).await.is_err() {
            return Err(ChannelError::ChannelClosed);
        }
    }
}
