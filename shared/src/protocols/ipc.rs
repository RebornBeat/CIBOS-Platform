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

/// Wire length of [`ChannelTermsWire`]: purpose(64) + purpose_len(1) +
/// direction(1) + pad(2) + max_message_bytes(4) + buffer_capacity(4) = 76 bytes.
pub const CHANNEL_TERMS_WIRE_LEN: usize = MAX_CHANNEL_PURPOSE + 1 + 1 + 2 + 4 + 4;

/// Fixed-size little-endian encoding of [`ChannelTerms`] for passing across the
/// syscall boundary by pointer (the 3-register ABI cannot carry the struct).
/// Mirrors the `FsRwArgs` wire convention. All multi-byte fields little-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelTermsWire {
    /// UTF-8 purpose bytes (only the first `purpose_len` are meaningful).
    pub purpose: [u8; MAX_CHANNEL_PURPOSE],
    /// Number of valid bytes in `purpose`.
    pub purpose_len: u8,
    /// `ChannelDirection` as its discriminant (1/2/3).
    pub direction: u8,
    /// Max bytes per message.
    pub max_message_bytes: u32,
    /// Buffered messages before back-pressure.
    pub buffer_capacity: u32,
}

impl ChannelTermsWire {
    /// Encode `terms` into the fixed wire layout.
    #[must_use]
    pub fn from_terms(terms: &ChannelTerms) -> Self {
        let mut purpose = [0u8; MAX_CHANNEL_PURPOSE];
        let bytes = terms.purpose.as_bytes();
        let n = bytes.len().min(MAX_CHANNEL_PURPOSE);
        purpose[..n].copy_from_slice(&bytes[..n]);
        Self {
            purpose,
            purpose_len: n as u8,
            direction: terms.direction as u8,
            max_message_bytes: terms.max_message_bytes,
            buffer_capacity: terms.buffer_capacity,
        }
    }

    /// Decode into validated [`ChannelTerms`]. Returns `None` if the purpose is
    /// not valid UTF-8/too long, the direction is unknown, or capacity is zero.
    #[must_use]
    pub fn to_terms(&self) -> Option<ChannelTerms> {
        let len = (self.purpose_len as usize).min(MAX_CHANNEL_PURPOSE);
        let purpose = core::str::from_utf8(&self.purpose[..len]).ok()?;
        let direction = match self.direction {
            1 => ChannelDirection::RequesterToReceiver,
            2 => ChannelDirection::ReceiverToRequester,
            3 => ChannelDirection::Bidirectional,
            _ => return None,
        };
        ChannelTerms::new(purpose, direction, self.max_message_bytes, self.buffer_capacity).ok()
    }

    /// Encode to the fixed little-endian byte layout.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; CHANNEL_TERMS_WIRE_LEN] {
        let mut b = [0u8; CHANNEL_TERMS_WIRE_LEN];
        b[..MAX_CHANNEL_PURPOSE].copy_from_slice(&self.purpose);
        let mut o = MAX_CHANNEL_PURPOSE;
        b[o] = self.purpose_len;
        o += 1;
        b[o] = self.direction;
        o += 3; // direction + 2 pad
        b[o..o + 4].copy_from_slice(&self.max_message_bytes.to_le_bytes());
        o += 4;
        b[o..o + 4].copy_from_slice(&self.buffer_capacity.to_le_bytes());
        b
    }

    /// Decode from the fixed little-endian byte layout.
    #[must_use]
    pub fn from_bytes(b: &[u8; CHANNEL_TERMS_WIRE_LEN]) -> Self {
        let mut purpose = [0u8; MAX_CHANNEL_PURPOSE];
        purpose.copy_from_slice(&b[..MAX_CHANNEL_PURPOSE]);
        let mut o = MAX_CHANNEL_PURPOSE;
        let purpose_len = b[o];
        o += 1;
        let direction = b[o];
        o += 3;
        let max_message_bytes = u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        o += 4;
        let buffer_capacity = u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        Self {
            purpose,
            purpose_len,
            direction,
            max_message_bytes,
            buffer_capacity,
        }
    }
}

/// Wire length of [`ChannelRequestWire`]: requester/target boundary (8) + terms.
pub const CHANNEL_REQUEST_WIRE_LEN: usize = 8 + CHANNEL_TERMS_WIRE_LEN;

/// What `poll_channel_request` writes into the receiver's buffer: the requesting
/// boundary plus the proposed terms, so the receiver can decide whether to
/// accept. Little-endian; the leading `u64` is the requester's boundary id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelRequestWire {
    /// The boundary that issued the request.
    pub requester: u64,
    /// The proposed terms.
    pub terms: ChannelTermsWire,
}

impl ChannelRequestWire {
    /// Encode to the fixed little-endian byte layout.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; CHANNEL_REQUEST_WIRE_LEN] {
        let mut b = [0u8; CHANNEL_REQUEST_WIRE_LEN];
        b[..8].copy_from_slice(&self.requester.to_le_bytes());
        b[8..].copy_from_slice(&self.terms.to_bytes());
        b
    }

    /// Decode from the fixed little-endian byte layout.
    #[must_use]
    pub fn from_bytes(b: &[u8; CHANNEL_REQUEST_WIRE_LEN]) -> Self {
        let requester = u64::from_le_bytes(b[..8].try_into().unwrap());
        let mut terms_bytes = [0u8; CHANNEL_TERMS_WIRE_LEN];
        terms_bytes.copy_from_slice(&b[8..]);
        Self {
            requester,
            terms: ChannelTermsWire::from_bytes(&terms_bytes),
        }
    }
}
