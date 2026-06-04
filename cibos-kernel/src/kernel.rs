//! # The Kernel
//!
//! Ties the [`Scheduler`] (which implements [`shared::KernelInterface`]) to the
//! [`LaneExecutor`] from the async runtime and runs the HIP scheduling loop.
//!
//! The loop is the concrete expression of Catch-and-Release:
//!
//! 1. Ask the scheduler for a dispatch batch (weighted-entropy selection over
//!    the ready lanes, bounded by execution contexts).
//! 2. Poll each selected lane through the executor.
//! 3. A lane that stalls registers a wait (catch) and leaves the ready set; a
//!    lane that yields re-signals ready; a lane that completes is reaped.
//! 4. Repeat until no lane is ready. Remaining stalled lanes wait for an
//!    external event (a timer maturing on [`Kernel::advance_clock`], or an I/O
//!    completion) to be released.
//!
//! The scheduler lock is held only inside the batch selection and the
//! `register_wait`/`signal_ready` calls — never across a poll — so polling a
//! lane that touches the kernel does not deadlock.

use crate::channel::{Channel, ChannelRegistry};
use crate::container::ContainerRegistry;
use crate::error::KernelResult;
use crate::memory::MemoryManager;
use crate::scheduler::Scheduler;
use alloc::sync::Arc;
use alloc::vec::Vec;
use cibos_async_runtime::{LaneExecutor, LanePoll};
use core::future::Future;
use core::time::Duration;
use shared::protocols::handoff::HandoffData;
use shared::protocols::ipc::ChannelTerms;
use shared::{
    BoundaryId, CibosProfile, KernelInterface, LaneId, MemoryRegion, ResourceLimits, SharedError,
    WeightClass,
};

/// The running kernel: scheduler, lane executor, memory accounting, and the
/// isolation-boundary registry.
pub struct Kernel {
    scheduler: Arc<Scheduler>,
    executor: LaneExecutor,
    memory: MemoryManager,
    containers: ContainerRegistry,
    channels: ChannelRegistry,
}

impl Kernel {
    /// Construct a kernel with `execution_contexts` execution contexts, seeded
    /// from `entropy_seed`, under `profile`, allowing up to `max_lanes`
    /// concurrent lanes.
    #[must_use]
    pub fn new(
        execution_contexts: usize,
        entropy_seed: [u8; 32],
        profile: CibosProfile,
        max_lanes: usize,
    ) -> Self {
        Self::assemble(
            execution_contexts,
            entropy_seed,
            profile,
            max_lanes,
            &[],
        )
    }

    /// Internal constructor shared by [`Kernel::new`] and [`Kernel::from_handoff`].
    fn assemble(
        execution_contexts: usize,
        entropy_seed: [u8; 32],
        profile: CibosProfile,
        max_lanes: usize,
        regions: &[MemoryRegion],
    ) -> Self {
        let scheduler = Arc::new(Scheduler::new(execution_contexts, entropy_seed, profile));
        let executor = LaneExecutor::new(scheduler.clone(), max_lanes);
        let memory = MemoryManager::from_regions(regions);
        let containers = ContainerRegistry::new();
        // The system boundary always exists. Its creation cannot fail on a fresh
        // registry, but we surface a clean panic message if invariants change.
        containers
            .create_system(ResourceLimits::default_application())
            .expect("fresh registry has no system boundary yet");
        Self {
            scheduler,
            executor,
            memory,
            containers,
            channels: ChannelRegistry::new(),
        }
    }

    /// Two-phase initialization from a firmware handoff record.
    ///
    /// Phase 1 (foundational): validate the handoff, derive the execution
    /// context count and entropy seed, build the memory map and scheduler.
    /// Phase 2 (structural): establish the system isolation boundary. The
    /// returned kernel is ready to spawn lanes and run.
    ///
    /// # Errors
    ///
    /// [`KernelError::Shared`] if the handoff fails validation (bad magic,
    /// version mismatch, forbidden profile pairing) or carries an invalid
    /// memory region.
    pub fn from_handoff(handoff: &HandoffData, max_lanes: usize) -> KernelResult<Self> {
        // Phase 1: validate and extract foundational facts.
        let decoded = handoff.validate().map_err(SharedError::from)?;
        let regions: Vec<MemoryRegion> = handoff
            .typed_regions()
            .map_err(SharedError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(SharedError::from)?;

        let contexts = decoded.topology.execution_contexts() as usize;

        // Phase 2 happens inside `assemble` (system boundary creation).
        Ok(Self::assemble(
            contexts,
            decoded.entropy_seed,
            decoded.cibos_profile,
            max_lanes,
            &regions,
        ))
    }

    /// The memory accounting subsystem.
    #[must_use]
    pub fn memory(&self) -> &MemoryManager {
        &self.memory
    }

    /// The isolation-boundary registry.
    #[must_use]
    pub fn containers(&self) -> &ContainerRegistry {
        &self.containers
    }

    /// Create a channel from negotiated `terms`, backed by this kernel's
    /// scheduler so its sends and receives integrate with Catch-and-Release.
    #[must_use]
    pub fn create_channel(&self, terms: &ChannelTerms) -> Channel {
        self.channels.create(terms, self.scheduler.clone())
    }

    /// The system boundary identifier.
    #[must_use]
    pub fn system_boundary(&self) -> BoundaryId {
        BoundaryId::SYSTEM
    }

    /// A handle to the kernel interface, for building resource-wait futures.
    #[must_use]
    pub fn interface(&self) -> Arc<dyn KernelInterface> {
        self.scheduler.clone()
    }

    /// The scheduler, for inspection and clock control.
    #[must_use]
    pub fn scheduler(&self) -> &Arc<Scheduler> {
        &self.scheduler
    }

    /// Number of lanes currently in flight.
    #[must_use]
    pub fn lane_count(&self) -> usize {
        self.executor.lane_count()
    }

    /// Whether the kernel has no ready and no stalled lanes.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.scheduler.is_idle()
    }

    /// Spawn a lane in the given weight `class`.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::KernelError::Runtime`] if the executor's lane limit
    /// is reached.
    pub fn spawn(
        &mut self,
        class: WeightClass,
        future: impl Future<Output = ()> + 'static,
    ) -> KernelResult<LaneId> {
        let lane = self.executor.spawn(future)?;
        self.scheduler.register_lane(lane, class);
        Ok(lane)
    }

    /// Spawn a lane whose future needs its own [`LaneId`] (for resource waits).
    ///
    /// # Errors
    ///
    /// Propagates [`crate::KernelError::Runtime`] if the executor's lane limit
    /// is reached.
    pub fn spawn_with_lane<F, Fut>(
        &mut self,
        class: WeightClass,
        build: F,
    ) -> KernelResult<LaneId>
    where
        F: FnOnce(LaneId) -> Fut,
        Fut: Future<Output = ()> + 'static,
    {
        let lane = self.executor.spawn_with_lane(build)?;
        self.scheduler.register_lane(lane, class);
        Ok(lane)
    }

    /// Install a future as a lane at a caller-supplied `lane` id (the host SDK
    /// transport mints ids itself), then register it with the scheduler.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::KernelError::Runtime`] if the executor's lane limit
    /// is reached.
    pub fn spawn_on(
        &mut self,
        lane: LaneId,
        class: WeightClass,
        future: impl Future<Output = ()> + 'static,
    ) -> KernelResult<()> {
        self.executor.spawn_on(lane, future)?;
        self.scheduler.register_lane(lane, class);
        Ok(())
    }

    /// Run the scheduling loop until no lane is ready, returning the number of
    /// lane polls performed. Stalled lanes (awaiting timers or I/O) remain;
    /// release them with [`Kernel::advance_clock`] or an external signal and
    /// call this again.
    pub fn run_until_idle(&mut self) -> usize {
        let mut polls = 0usize;
        while self.scheduler.has_ready() {
            let batch = self.scheduler.take_dispatch_batch();
            for lane in batch {
                match self.executor.poll_lane(lane) {
                    LanePoll::Completed | LanePoll::NotFound => {
                        self.scheduler.notify_complete(lane);
                    }
                    LanePoll::Pending => {}
                }
                polls += 1;
                debug_assert!(polls < 10_000_000, "runaway scheduling loop");
            }
        }
        polls
    }

    /// Advance the monotonic clock, releasing matured timer waits.
    pub fn advance_clock(&self, delta: Duration) {
        self.scheduler.advance_clock(delta);
    }

    /// Idle pump for the host transport: advance the clock to the next pending
    /// timer deadline (releasing matured timer waits), returning `true` if a
    /// timer was found and the clock advanced, or `false` if no timer waits
    /// remain. See [`Scheduler::advance_to_next_timer`].
    pub fn advance_to_next_timer(&self) -> bool {
        self.scheduler.advance_to_next_timer()
    }
}
