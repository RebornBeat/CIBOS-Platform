//! # The Lane Executor
//!
//! [`LaneExecutor`] owns the in-flight lanes (each a boxed, pinned future),
//! holds each lane's [`Waker`], and polls a lane on demand.
//!
//! It does **not** contain a scheduling loop. Under HIP the kernel is the
//! scheduler: its selector decides which ready lane to run and calls
//! [`LaneExecutor::poll_lane`]. The executor's responsibilities are narrow and
//! mechanical — store futures, hand out wakers, poll when told, and reap
//! completed lanes — which keeps scheduling policy entirely in the kernel where
//! the HIP guarantees are enforced.
//!
//! Spawning a lane requests its first poll through
//! [`KernelInterface::signal_ready`], so a freshly spawned lane is scheduled the
//! same way a woken one is: there is a single path by which lanes become
//! runnable.

use crate::error::{RuntimeError, RuntimeResult};
use crate::waker::CibosWaker;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use shared::{KernelInterface, LaneId};

/// The outcome of polling a single lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanePoll {
    /// The lane's future completed and the lane has been removed.
    Completed,
    /// The lane is still pending (it stalled on a resource or yielded).
    Pending,
    /// No lane with the given identifier exists.
    NotFound,
}

/// One in-flight lane: its future and its waker.
struct Lane {
    future: Pin<Box<dyn Future<Output = ()>>>,
    waker: core::task::Waker,
}

/// Owns and polls lanes on behalf of the kernel scheduler.
pub struct LaneExecutor {
    kernel: Arc<dyn KernelInterface>,
    lanes: BTreeMap<LaneId, Lane>,
    next_id: u64,
    max_lanes: usize,
}

impl LaneExecutor {
    /// Create an executor backed by `kernel`, with a ceiling of `max_lanes`
    /// concurrent lanes.
    #[must_use]
    pub fn new(kernel: Arc<dyn KernelInterface>, max_lanes: usize) -> Self {
        Self {
            kernel,
            lanes: BTreeMap::new(),
            next_id: 1,
            max_lanes,
        }
    }

    /// Number of lanes currently in flight.
    #[must_use]
    pub fn lane_count(&self) -> usize {
        self.lanes.len()
    }

    /// Whether a lane with `id` is currently in flight.
    #[must_use]
    pub fn has_lane(&self, id: LaneId) -> bool {
        self.lanes.contains_key(&id)
    }

    /// Spawn `future` as a new lane, returning its identifier. Requests the
    /// lane's first poll through the kernel, so it enters scheduling the same
    /// way a woken lane does.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::LaneLimitExceeded`] if the executor is at `max_lanes`.
    pub fn spawn(
        &mut self,
        future: impl Future<Output = ()> + 'static,
    ) -> RuntimeResult<LaneId> {
        self.spawn_with_lane(|_| future)
    }

    /// Spawn a lane whose future needs to know its own [`LaneId`] — for example,
    /// to construct a [`crate::future::ResourceWait`] for itself. The closure
    /// receives the assigned identifier and returns the future.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::LaneLimitExceeded`] if the executor is at `max_lanes`.
    pub fn spawn_with_lane<F, Fut>(&mut self, build: F) -> RuntimeResult<LaneId>
    where
        F: FnOnce(LaneId) -> Fut,
        Fut: Future<Output = ()> + 'static,
    {
        if self.lanes.len() >= self.max_lanes {
            return Err(RuntimeError::LaneLimitExceeded {
                limit: self.max_lanes,
            });
        }
        let lane = LaneId::new(self.next_id);
        self.next_id += 1;

        let waker = CibosWaker::waker_for(lane, self.kernel.clone());
        let future = Box::pin(build(lane));
        self.lanes.insert(lane, Lane { future, waker });

        // Request the first poll through the normal readiness path.
        self.kernel.signal_ready(lane);
        Ok(lane)
    }

    /// Install `future` as a lane at a caller-supplied `lane` id, rather than one
    /// minted here. Used by the host SDK transport, where the SDK is the lane-id
    /// authority (so ids never collide with [`spawn`]/[`spawn_with_lane`]).
    /// Requests the first poll through the kernel, like the other spawns.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::LaneLimitExceeded`] if the executor is at `max_lanes`.
    ///
    /// # Panics
    ///
    /// Debug-asserts that `lane` is not already in flight; the SDK mints unique
    /// ascending ids, so a collision indicates a transport bug.
    ///
    /// [`spawn`]: LaneExecutor::spawn
    /// [`spawn_with_lane`]: LaneExecutor::spawn_with_lane
    pub fn spawn_on(
        &mut self,
        lane: LaneId,
        future: impl Future<Output = ()> + 'static,
    ) -> RuntimeResult<()> {
        if self.lanes.len() >= self.max_lanes {
            return Err(RuntimeError::LaneLimitExceeded {
                limit: self.max_lanes,
            });
        }
        debug_assert!(
            !self.lanes.contains_key(&lane),
            "spawn_on called with an already-active lane id",
        );

        let waker = CibosWaker::waker_for(lane, self.kernel.clone());
        let future = Box::pin(future);
        self.lanes.insert(lane, Lane { future, waker });

        self.kernel.signal_ready(lane);
        Ok(())
    }

    /// Poll one lane once. Called by the kernel scheduler when it selects a
    /// ready lane. Reaps the lane if its future completes.
    pub fn poll_lane(&mut self, lane: LaneId) -> LanePoll {
        let entry = match self.lanes.get_mut(&lane) {
            Some(e) => e,
            None => return LanePoll::NotFound,
        };
        let mut cx = Context::from_waker(&entry.waker);
        match entry.future.as_mut().poll(&mut cx) {
            Poll::Ready(()) => {
                self.lanes.remove(&lane);
                LanePoll::Completed
            }
            Poll::Pending => LanePoll::Pending,
        }
    }

    /// Abort a lane, dropping its future without completing it. Returns whether
    /// a lane was removed.
    pub fn abort(&mut self, lane: LaneId) -> bool {
        self.lanes.remove(&lane).is_some()
    }
}
