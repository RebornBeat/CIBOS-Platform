//! Multi-container host (development/test transport).
//!
//! [`MultiContainerHost`] runs several containers over a single in-process
//! [`Kernel`]. Each container has its own [`System`] — its own isolation
//! [`BoundaryId`](shared::BoundaryId), resource limits, and lane/channel
//! accounting — while sharing one executor and scheduler. The ambient execution
//! context is per lane (installed by the lane wrapper), so a lane always sees the
//! `System` of the container it belongs to; `container::id`, `channel_count`,
//! and lane/channel limits are therefore per container.
//!
//! This is the substrate for cross-container work: with more than one container
//! present, one container can address another by its `BoundaryId`. A single
//! application is better served by [`AppHost`](crate::AppHost).

use crate::broker::ChannelBroker;
use crate::system::{IdSource, System};
use cibos_kernel::Kernel;
use shared::{CibosProfile, ResourceLimits, WeightClass};
use std::rc::Rc;

/// Runs several containers over one in-process kernel.
pub struct MultiContainerHost {
    kernel: Kernel,
    systems: Vec<System>,
    ids: Rc<IdSource>,
    broker: Rc<ChannelBroker>,
}

impl MultiContainerHost {
    /// Create a host with `execution_contexts` contexts, seeded from `seed`,
    /// under `profile`, allowing up to `max_lanes` lanes across all containers.
    #[must_use]
    pub fn new(
        execution_contexts: usize,
        seed: [u8; 32],
        profile: CibosProfile,
        max_lanes: usize,
    ) -> Self {
        let kernel = Kernel::new(execution_contexts, seed, profile, max_lanes);
        MultiContainerHost {
            kernel,
            systems: Vec::new(),
            ids: IdSource::new(),
            broker: ChannelBroker::new(),
        }
    }

    /// Register a new container with its own `limits`, returning its [`System`].
    /// The container is assigned a fresh isolation boundary in the kernel
    /// registry; spawn its initial lanes through the returned handle.
    pub fn add_container(&mut self, limits: ResourceLimits) -> System {
        let boundary = self.kernel.containers().create(limits, WeightClass::User);
        let system = System::new(
            self.kernel.interface(),
            limits,
            boundary,
            self.ids.clone(),
            self.broker.clone(),
        );
        self.systems.push(system.clone());
        system
    }

    /// Drive every container until the whole system is idle. Returns the total
    /// number of lane polls performed.
    ///
    /// No single ambient system is installed: each lane installs its owning
    /// container's system as it is polled, so the documented free-function API
    /// resolves to the right container per lane.
    pub fn run(&mut self) -> usize {
        let mut total_polls = 0usize;
        loop {
            // Install every container's queued spawns at their minted lane ids.
            for system in &self.systems {
                for spawn in system.take_pending() {
                    if self
                        .kernel
                        .spawn_on(spawn.lane, spawn.class, spawn.future)
                        .is_err()
                    {
                        system.note_lane_complete();
                    }
                }
            }

            total_polls += self.kernel.run_until_idle();

            let all_idle = self.systems.iter().all(System::pending_is_empty);
            if all_idle && !self.kernel.scheduler().has_ready() {
                if self.kernel.advance_to_next_timer() {
                    continue;
                }
                break;
            }
        }
        total_polls
    }

    /// Borrow the underlying kernel (for advancing the clock, inspection, etc.).
    #[must_use]
    pub fn kernel(&self) -> &Kernel {
        &self.kernel
    }

    /// Advance the kernel clock and continue running (releases matured timers).
    pub fn advance_and_run(&mut self, delta: core::time::Duration) -> usize {
        self.kernel.advance_clock(delta);
        self.run()
    }
}
