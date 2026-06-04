//! Container self-inspection (API Reference, Chapter 5).
//!
//! [`get_resource_limits`] and [`memory_limit`] read the current container's
//! limits from the ambient [execution context](crate::context); limits are
//! fixed at launch by the deployer, so they are reported exactly. [`id`]
//! returns the container's real isolation [`BoundaryId`] — the host runner
//! registers each application as a user container in the kernel's isolation
//! registry, and that boundary is the application's container id.
//!
//! The usage-reporting calls `memory_usage()` and `channel_count()` are not yet
//! provided: the kernel's container registry can track allocation, peak, lane,
//! and channel counts, but the host transport does not yet route the live
//! lane/channel/allocation events into the registry, so it would report zeroes.
//! Rather than report placeholder figures, those calls are added with that live
//! accounting (which also needs allocation tracking the host does not perform).

use crate::context::current_system;
use shared::ResourceLimits;

/// An isolation boundary identifier — a container's id.
pub use shared::BoundaryId as ContainerId;

/// This container's resource limits: memory ceiling, maximum lanes, maximum
/// channels, and the per-channel message-size and buffer caps.
///
/// # Panics
///
/// Panics if called outside a running application (no ambient system).
#[must_use]
pub fn get_resource_limits() -> ResourceLimits {
    current_system().resource_limits()
}

/// The maximum number of bytes this container may have allocated at once.
///
/// # Panics
///
/// Panics if called outside a running application (no ambient system).
#[must_use]
pub fn memory_limit() -> usize {
    current_system().resource_limits().memory_bytes as usize
}

/// This container's id — the isolation [`ContainerId`] the application runs in.
///
/// On the host transport each application is registered as a user container in
/// the kernel's isolation registry; this returns that container's boundary.
///
/// # Panics
///
/// Panics if called outside a running application (no ambient system).
#[must_use]
pub fn id() -> ContainerId {
    current_system().boundary()
}

/// A breakdown of the container's currently open channels by direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelCount {
    /// Channels a remote container opened to this one.
    pub inbound: u32,
    /// Channels this container opened to a remote one.
    pub outbound: u32,
    /// Local (intra-container) channels opened via `Channel::new_local`.
    pub local: u32,
}

/// This container's open channels, by direction.
///
/// `local` counts the `Channel::new_local` channels currently open (cloned
/// handles of one channel count once; a channel stops counting when closed or
/// when all its handles drop). `outbound` counts cross-container channels this
/// container requested ([`Channel::request`](crate::Channel::request)) and
/// `inbound` those it accepted ([`await_channel_request`]). Internal channels the
/// SDK opens for plumbing (lane joins, doorbells, rendezvous) are not counted.
///
/// # Panics
///
/// Panics if called outside a running application (no ambient system).
#[must_use]
pub fn channel_count() -> ChannelCount {
    let system = current_system();
    ChannelCount {
        inbound: system.inbound_channel_count(),
        outbound: system.outbound_channel_count(),
        local: system.local_channel_count(),
    }
}

/// Wait for an incoming cross-container channel request to this container.
/// See [`await_channel_request`](crate::channel::await_channel_request).
pub use crate::channel::{await_channel_request, IncomingRequest};

/// A snapshot of the container's memory accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryStats {
    /// Bytes currently allocated.
    pub allocated_bytes: u64,
    /// The high-water mark of allocated bytes.
    pub peak_bytes: u64,
    /// The container's memory ceiling (see [`memory_limit`]).
    pub limit_bytes: u64,
}

/// This container's current memory usage: bytes allocated now, the peak so far,
/// and the limit.
///
/// On the host transport this is available only with the `host-memory-tracking`
/// feature, which installs a counting global allocator; `allocated_bytes` and
/// `peak_bytes` are then the process's live and peak heap totals (the host runs
/// the application as a single in-process container). In the production
/// transport the kernel tracks this directly.
///
/// # Panics
///
/// Panics if called outside a running application (no ambient system).
#[cfg(feature = "host-memory-tracking")]
#[must_use]
pub fn memory_usage() -> MemoryStats {
    MemoryStats {
        allocated_bytes: crate::tracking::allocated_bytes() as u64,
        peak_bytes: crate::tracking::peak_bytes() as u64,
        limit_bytes: current_system().resource_limits().memory_bytes,
    }
}
