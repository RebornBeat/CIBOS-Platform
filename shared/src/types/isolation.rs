//! # Isolation Boundary Types
//!
//! Types describing the isolation boundaries the system enforces.
//!
//! A core principle of CIBIOS/CIBOS is that isolation is **not** a tunable
//! security gradient — every container is fully isolated, always. There is no
//! "low isolation" mode. Consequently the types here do not describe *how much*
//! isolation a unit gets; they describe *which resources* an already-fully-
//! isolated unit is permitted to use, and how the boundary is identified.
//!
//! These types are shared between the kernel (which creates and enforces
//! boundaries) and the SDK (through which an application queries its own
//! limits), so they live in `shared`.

use crate::types::error::ResourceError;

/// Unique identifier for an isolation boundary.
///
/// In practice a boundary corresponds to a container. The identifier is opaque
/// and globally unique for the lifetime of the boundary; it is never reused
/// while the boundary is live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct BoundaryId(pub u64);

impl BoundaryId {
    /// The kernel/system boundary identifier (reserved).
    pub const SYSTEM: BoundaryId = BoundaryId(0);

    /// Construct a boundary identifier from a raw value.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        BoundaryId(raw)
    }

    /// The raw underlying value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Whether this is the reserved system boundary.
    #[must_use]
    pub const fn is_system(self) -> bool {
        self.0 == 0
    }
}

/// Unique identifier for a lane (an isolated execution pathway within a
/// container's boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct LaneId(pub u64);

impl LaneId {
    /// Construct a lane identifier from a raw value.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        LaneId(raw)
    }

    /// The raw underlying value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Unique identifier for a communication channel between (or within)
/// boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct ChannelId(pub u64);

impl ChannelId {
    /// Construct a channel identifier from a raw value.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        ChannelId(raw)
    }

    /// The raw underlying value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// The resource ceilings imposed on a single isolated container.
///
/// These mirror the limits an application can query at runtime through the SDK
/// (`container::get_resource_limits()`). Exceeding a limit does not crash the
/// container; per the HIP model the offending operation stalls (for memory and
/// channel buffers) under Catch-and-Release, except for hard allocation caps
/// which surface as a [`ResourceError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Maximum bytes the container may have allocated at once.
    pub memory_bytes: u64,
    /// Maximum number of lanes the container may create.
    pub max_lanes: u32,
    /// Maximum number of channels (inbound + outbound + local).
    pub max_channels: u32,
    /// Maximum size of a single channel message, in bytes.
    pub max_message_bytes: u32,
    /// Maximum buffered messages per channel.
    pub max_channel_buffer: u32,
}

impl ResourceLimits {
    /// A conservative default suitable for a typical application container.
    ///
    /// 512 MiB of memory, 256 lanes, 16 channels, 64 KiB messages, 256-deep
    /// channel buffers. These match the documented compiled defaults.
    #[must_use]
    pub const fn default_application() -> Self {
        Self {
            memory_bytes: 512 * 1024 * 1024,
            max_lanes: 256,
            max_channels: 16,
            max_message_bytes: 64 * 1024,
            max_channel_buffer: 256,
        }
    }

    /// Validate that a requested allocation fits within the memory limit.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError::MemoryLimitExceeded`] when `requested` plus
    /// `already_allocated` would exceed [`Self::memory_bytes`].
    pub const fn check_allocation(
        &self,
        already_allocated: u64,
        requested: u64,
    ) -> Result<(), ResourceError> {
        // saturating add avoids overflow turning an over-limit request into a
        // spuriously-accepted one.
        let total = already_allocated.saturating_add(requested);
        if total > self.memory_bytes {
            Err(ResourceError::MemoryLimitExceeded {
                requested,
                limit: self.memory_bytes,
            })
        } else {
            Ok(())
        }
    }
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self::default_application()
    }
}

/// Current resource usage of a container, reported by the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResourceUsage {
    /// Bytes currently allocated.
    pub allocated_bytes: u64,
    /// Peak bytes allocated since the container started.
    pub peak_bytes: u64,
    /// Number of lanes currently in existence.
    pub lane_count: u32,
    /// Number of channels currently open.
    pub channel_count: u32,
}

/// The complete configuration of one isolation boundary, as established when a
/// container is launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundaryConfiguration {
    /// Identifier assigned to this boundary.
    pub id: BoundaryId,
    /// Resource ceilings for the boundary.
    pub limits: ResourceLimits,
    /// Weight class governing scheduling probability under competition.
    pub weight_class: WeightClass,
}

impl BoundaryConfiguration {
    /// Construct a boundary configuration.
    #[must_use]
    pub const fn new(id: BoundaryId, limits: ResourceLimits, weight_class: WeightClass) -> Self {
        Self {
            id,
            limits,
            weight_class,
        }
    }
}

/// Scheduling weight class for a container's lanes.
///
/// Under the HIP weighted-entropy selector, weight only affects dispatch
/// probability when competition exists (more ready events than execution
/// contexts). When there is no competition all ready events dispatch
/// simultaneously and the class is irrelevant. Under the Maximum Isolation
/// profile all classes are forced equal for non-determinism, so this value is
/// advisory and may be ignored by the active profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum WeightClass {
    /// System services: window managers, input handlers, compositors.
    System = 1,
    /// Standard user applications.
    User = 2,
    /// Non-critical background work.
    Background = 3,
}

impl WeightClass {
    /// The raw `u32` discriminant.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}
