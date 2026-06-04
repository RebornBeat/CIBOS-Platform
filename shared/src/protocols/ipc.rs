//! # Inter-Process Communication Protocol
//!
//! The boundary between the HIP async runtime and the kernel scheduler, plus
//! the channel-establishment vocabulary.
//!
//! ## The kernel interface
//!
//! [`KernelInterface`] is the seam the custom async runtime is built on. When a
//! future returns `Poll::Pending`, the runtime calls
//! [`KernelInterface::register_wait`] to tell the kernel which resource the lane
//! is blocked on; the lane then sits in the Stalled List. When the resource
//! becomes available the kernel wakes the lane, the waker calls
//! [`KernelInterface::signal_ready`], and the kernel re-qualifies and dispatches
//! the lane. No polling, no spinning — this trait is exactly the Catch-and-
//! Release mechanism expressed as a callable interface.
//!
//! The trait lives in `shared` (not in the kernel or the runtime) precisely
//! because both the kernel — which *implements* it — and the runtime — which
//! *consumes* it — must agree on its shape without depending on each other.
//!
//! ## Channels
//!
//! [`ChannelTerms`] describes a proposed point-to-point channel. Terms are
//! proposed by the requester and accepted-or-rejected wholesale by the
//! receiver; there is no counter-proposal, which removes negotiation timing as
//! an observable signal.

use crate::types::isolation::{ChannelId, LaneId};
use crate::types::time::Monotonic;
use heapless::String as HeaplessString;

/// A resource a lane can stall waiting for, under Catch-and-Release.
///
/// When a future cannot make progress it reports the specific resource it needs
/// through this enum. The kernel records the dependency and only re-qualifies
/// the lane once that resource (and every other resource the lane needs) is
/// available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitResource {
    /// Data is available to receive on the given channel.
    ChannelData(ChannelId),
    /// Buffer space is available to send on the given channel.
    ChannelBuffer(ChannelId),
    /// A memory allocation of the given size (bytes) can be satisfied.
    Memory(u64),
    /// A disk I/O operation, identified by its handle, has completed.
    DiskIo(u64),
    /// A network I/O operation, identified by its handle, has completed.
    NetworkIo(u64),
    /// A timer deadline has been reached.
    Timer(Monotonic),
}

/// The interface the kernel exposes to the async runtime.
///
/// Implemented by the kernel; consumed by the runtime's wakers and futures.
/// Must be `Send + Sync` because the runtime shares it across execution
/// contexts via an atomically reference-counted handle.
///
/// The weight-control methods have default implementations that report "not
/// applied" (`false`). Profiles that do not compile per-lane or dynamic weights
/// (Maximum Isolation, Balanced, Performance) simply leave the defaults in
/// place; only Compute overrides them. This keeps weight control out of the
/// cross-crate feature surface while still honoring the profile rules.
pub trait KernelInterface: Send + Sync {
    /// Record that `lane` is waiting on `resource`. Called when a future
    /// returns `Poll::Pending`. Does not block; the lane moves to the Stalled
    /// List and the call returns immediately (triggering, not coordination).
    fn register_wait(&self, lane: LaneId, resource: WaitResource);

    /// Signal that `lane` has work ready and should be considered for dispatch.
    /// Called by a waker when a resource becomes available. The kernel decides
    /// *when* to actually poll, per Catch-and-Release.
    fn signal_ready(&self, lane: LaneId);

    /// The current monotonic time, for timer futures and wait accounting.
    fn now(&self) -> Monotonic;

    /// Set a lane's scheduling weight at creation (per-lane-weights).
    ///
    /// Returns `true` if applied, `false` if the active profile does not
    /// support per-lane weights. Default: not supported.
    fn set_lane_weight(&self, lane: LaneId, weight: u32) -> bool {
        let _ = (lane, weight);
        false
    }

    /// Update a lane's scheduling weight at runtime (dynamic-weights).
    ///
    /// Returns `true` if applied, `false` if the active profile does not
    /// support dynamic weights. Default: not supported.
    fn update_lane_weight(&self, lane: LaneId, weight: u32) -> bool {
        let _ = (lane, weight);
        false
    }
}

/// Maximum length, in bytes, of a channel purpose string.
pub const MAX_CHANNEL_PURPOSE: usize = 64;

/// A human-readable channel purpose with fixed capacity.
pub type ChannelPurpose = HeaplessString<MAX_CHANNEL_PURPOSE>;

/// The direction data may flow on a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum ChannelDirection {
    /// Requester sends, receiver receives.
    RequesterToReceiver = 1,
    /// Receiver sends, requester receives.
    ReceiverToRequester = 2,
    /// Both directions.
    Bidirectional = 3,
}

/// Proposed terms for a channel. Proposed by the requester; the receiver
/// accepts all of them or rejects the request entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelTerms {
    /// What the channel is for. Helps the receiver decide whether to accept.
    pub purpose: ChannelPurpose,
    /// Direction data may flow.
    pub direction: ChannelDirection,
    /// Maximum size of a single message, in bytes. The kernel rejects larger.
    pub max_message_bytes: u32,
    /// Maximum number of messages buffered in-kernel before back-pressure.
    pub buffer_capacity: u32,
}

impl ChannelTerms {
    /// Construct channel terms, validating the purpose length and the numeric
    /// bounds.
    ///
    /// # Errors
    ///
    /// Returns [`crate::types::error::ProtocolError::ChannelEstablishmentFailed`]
    /// if `purpose` is too long, or if `buffer_capacity` is zero.
    pub fn new(
        purpose: &str,
        direction: ChannelDirection,
        max_message_bytes: u32,
        buffer_capacity: u32,
    ) -> Result<Self, crate::types::error::ProtocolError> {
        use crate::types::error::ProtocolError;
        let purpose = ChannelPurpose::try_from(purpose).map_err(|()| {
            ProtocolError::ChannelEstablishmentFailed {
                detail: "purpose string too long",
            }
        })?;
        if buffer_capacity == 0 {
            return Err(ProtocolError::ChannelEstablishmentFailed {
                detail: "buffer capacity must be non-zero",
            });
        }
        Ok(Self {
            purpose,
            direction,
            max_message_bytes,
            buffer_capacity,
        })
    }
}

/// A request to establish a channel from one boundary to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRequest {
    /// The boundary the requester wishes to reach.
    pub target: crate::types::isolation::BoundaryId,
    /// The terms proposed.
    pub terms: ChannelTerms,
}

/// The outcome of a channel request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelAcceptance {
    /// The receiver accepted; the channel now exists with this identifier.
    Accepted(ChannelId),
    /// The receiver rejected the request.
    Rejected,
}
