//! # Container / Boundary Management
//!
//! A *container* is one isolation boundary: a unit that owns lanes and channels
//! and has its own [`ResourceLimits`]. This module is where those limits are
//! enforced — every lane creation, channel creation, and memory allocation a
//! boundary makes is checked here against its ceiling.
//!
//! Isolation is binary: every container is fully isolated. The registry does
//! not model degrees of isolation, only ownership and resource accounting. The
//! reserved system boundary [`BoundaryId::SYSTEM`] is created at kernel bringup.

use crate::error::{KernelError, KernelResult};
use crate::sync::SpinLock;
use alloc::collections::{BTreeMap, BTreeSet};
use shared::{
    BoundaryConfiguration, BoundaryId, ChannelId, LaneId, ResourceLimits, ResourceUsage,
    WeightClass,
};

/// One isolation boundary and the resources it owns.
struct Container {
    config: BoundaryConfiguration,
    lanes: BTreeSet<LaneId>,
    channels: BTreeSet<ChannelId>,
    allocated_bytes: u64,
    peak_bytes: u64,
}

impl Container {
    fn usage(&self) -> ResourceUsage {
        ResourceUsage {
            allocated_bytes: self.allocated_bytes,
            peak_bytes: self.peak_bytes,
            lane_count: self.lanes.len() as u32,
            channel_count: self.channels.len() as u32,
        }
    }
}

struct RegistryState {
    containers: BTreeMap<BoundaryId, Container>,
    next_id: u64,
}

/// The registry of all isolation boundaries.
pub struct ContainerRegistry {
    state: SpinLock<RegistryState>,
}

impl ContainerRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: SpinLock::new(RegistryState {
                containers: BTreeMap::new(),
                next_id: 1, // 0 is reserved for the system boundary
            }),
        }
    }

    /// Create the reserved system boundary ([`BoundaryId::SYSTEM`]).
    ///
    /// # Errors
    ///
    /// [`KernelError::InitFailed`] if the system boundary already exists.
    pub fn create_system(&self, limits: ResourceLimits) -> KernelResult<BoundaryId> {
        let mut s = self.state.lock();
        if s.containers.contains_key(&BoundaryId::SYSTEM) {
            return Err(KernelError::InitFailed {
                phase: "system boundary already exists",
            });
        }
        let config = BoundaryConfiguration::new(BoundaryId::SYSTEM, limits, WeightClass::System);
        s.containers.insert(
            BoundaryId::SYSTEM,
            Container {
                config,
                lanes: BTreeSet::new(),
                channels: BTreeSet::new(),
                allocated_bytes: 0,
                peak_bytes: 0,
            },
        );
        Ok(BoundaryId::SYSTEM)
    }

    /// Create a new user container with the given limits and weight class.
    #[must_use]
    pub fn create(&self, limits: ResourceLimits, weight_class: WeightClass) -> BoundaryId {
        let mut s = self.state.lock();
        let id = BoundaryId::new(s.next_id);
        s.next_id += 1;
        let config = BoundaryConfiguration::new(id, limits, weight_class);
        s.containers.insert(
            id,
            Container {
                config,
                lanes: BTreeSet::new(),
                channels: BTreeSet::new(),
                allocated_bytes: 0,
                peak_bytes: 0,
            },
        );
        id
    }

    /// Destroy a container, returning its owned lane and channel ids so the
    /// caller can tear them down. Memory release is the caller's responsibility
    /// (via the memory manager).
    ///
    /// # Errors
    ///
    /// [`KernelError::UnknownContainer`] if no such container exists.
    pub fn destroy(&self, id: BoundaryId) -> KernelResult<()> {
        let mut s = self.state.lock();
        s.containers
            .remove(&id)
            .map(|_| ())
            .ok_or(KernelError::UnknownContainer)
    }

    /// Whether a container exists.
    #[must_use]
    pub fn contains(&self, id: BoundaryId) -> bool {
        self.state.lock().containers.contains_key(&id)
    }

    /// Number of containers.
    #[must_use]
    pub fn count(&self) -> usize {
        self.state.lock().containers.len()
    }

    /// Add a lane to a container, enforcing its lane ceiling.
    ///
    /// # Errors
    ///
    /// [`KernelError::UnknownContainer`] or [`KernelError::LimitExceeded`].
    pub fn add_lane(&self, id: BoundaryId, lane: LaneId) -> KernelResult<()> {
        let mut s = self.state.lock();
        let c = s.containers.get_mut(&id).ok_or(KernelError::UnknownContainer)?;
        if c.lanes.len() as u32 >= c.config.limits.max_lanes {
            return Err(KernelError::LimitExceeded { resource: "lanes" });
        }
        c.lanes.insert(lane);
        Ok(())
    }

    /// Remove a lane from a container.
    pub fn remove_lane(&self, id: BoundaryId, lane: LaneId) {
        if let Some(c) = self.state.lock().containers.get_mut(&id) {
            c.lanes.remove(&lane);
        }
    }

    /// Add a channel to a container, enforcing its channel ceiling.
    ///
    /// # Errors
    ///
    /// [`KernelError::UnknownContainer`] or [`KernelError::LimitExceeded`].
    pub fn add_channel(&self, id: BoundaryId, channel: ChannelId) -> KernelResult<()> {
        let mut s = self.state.lock();
        let c = s.containers.get_mut(&id).ok_or(KernelError::UnknownContainer)?;
        if c.channels.len() as u32 >= c.config.limits.max_channels {
            return Err(KernelError::LimitExceeded {
                resource: "channels",
            });
        }
        c.channels.insert(channel);
        Ok(())
    }

    /// Remove a channel from a container.
    pub fn remove_channel(&self, id: BoundaryId, channel: ChannelId) {
        if let Some(c) = self.state.lock().containers.get_mut(&id) {
            c.channels.remove(&channel);
        }
    }

    /// Account a memory allocation against a container's limit.
    ///
    /// # Errors
    ///
    /// [`KernelError::UnknownContainer`], or [`KernelError::LimitExceeded`] if
    /// the allocation would exceed the container's memory ceiling.
    pub fn allocate(&self, id: BoundaryId, bytes: u64) -> KernelResult<()> {
        let mut s = self.state.lock();
        let c = s.containers.get_mut(&id).ok_or(KernelError::UnknownContainer)?;
        c.config
            .limits
            .check_allocation(c.allocated_bytes, bytes)
            .map_err(|e| KernelError::from(shared::SharedError::from(e)))?;
        c.allocated_bytes += bytes;
        if c.allocated_bytes > c.peak_bytes {
            c.peak_bytes = c.allocated_bytes;
        }
        Ok(())
    }

    /// Release previously-allocated memory back to a container's budget.
    pub fn release_memory(&self, id: BoundaryId, bytes: u64) {
        if let Some(c) = self.state.lock().containers.get_mut(&id) {
            c.allocated_bytes = c.allocated_bytes.saturating_sub(bytes);
        }
    }

    /// Current resource usage of a container, if it exists.
    #[must_use]
    pub fn usage(&self, id: BoundaryId) -> Option<ResourceUsage> {
        self.state.lock().containers.get(&id).map(Container::usage)
    }
}

impl Default for ContainerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_limits() -> ResourceLimits {
        ResourceLimits {
            memory_bytes: 1024,
            max_lanes: 2,
            max_channels: 1,
            max_message_bytes: 256,
            max_channel_buffer: 8,
        }
    }

    #[test]
    fn create_and_destroy() {
        let reg = ContainerRegistry::new();
        let id = reg.create(small_limits(), WeightClass::User);
        assert!(reg.contains(id));
        assert_eq!(reg.count(), 1);
        reg.destroy(id).unwrap();
        assert!(!reg.contains(id));
        assert!(matches!(reg.destroy(id), Err(KernelError::UnknownContainer)));
    }

    #[test]
    fn system_boundary_is_unique() {
        let reg = ContainerRegistry::new();
        reg.create_system(ResourceLimits::default_application()).unwrap();
        assert!(reg.contains(BoundaryId::SYSTEM));
        assert!(reg.create_system(ResourceLimits::default_application()).is_err());
    }

    #[test]
    fn lane_ceiling_enforced() {
        let reg = ContainerRegistry::new();
        let id = reg.create(small_limits(), WeightClass::User);
        reg.add_lane(id, LaneId::new(1)).unwrap();
        reg.add_lane(id, LaneId::new(2)).unwrap();
        assert!(matches!(
            reg.add_lane(id, LaneId::new(3)),
            Err(KernelError::LimitExceeded { resource: "lanes" })
        ));
        reg.remove_lane(id, LaneId::new(1));
        // Slot freed: another lane fits.
        reg.add_lane(id, LaneId::new(3)).unwrap();
    }

    #[test]
    fn channel_ceiling_enforced() {
        let reg = ContainerRegistry::new();
        let id = reg.create(small_limits(), WeightClass::User);
        reg.add_channel(id, ChannelId::new(1)).unwrap();
        assert!(matches!(
            reg.add_channel(id, ChannelId::new(2)),
            Err(KernelError::LimitExceeded { resource: "channels" })
        ));
    }

    #[test]
    fn memory_ceiling_enforced() {
        let reg = ContainerRegistry::new();
        let id = reg.create(small_limits(), WeightClass::User);
        reg.allocate(id, 512).unwrap();
        reg.allocate(id, 512).unwrap();
        // 1024 used; one more byte exceeds the 1024 limit.
        assert!(reg.allocate(id, 1).is_err());
        let usage = reg.usage(id).unwrap();
        assert_eq!(usage.allocated_bytes, 1024);
        assert_eq!(usage.peak_bytes, 1024);
        // Release and reallocate.
        reg.release_memory(id, 512);
        assert_eq!(reg.usage(id).unwrap().allocated_bytes, 512);
        reg.allocate(id, 256).unwrap();
        assert_eq!(reg.usage(id).unwrap().peak_bytes, 1024, "peak retained");
    }
}
