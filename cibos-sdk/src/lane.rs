//! The Lane API (API Reference, Chapter 1).
//!
//! A [`Lane`] is a unit of concurrency within a container. [`Lane::create`]
//! reserves a lane (and its id) up front; [`Lane::submit`] runs one future on it
//! at a time; [`Lane::join`] waits for the running future to finish; and
//! [`Lane::destroy`] releases the lane. The same lane can be reused: after a
//! `join`, another future may be `submit`ted.
//!
//! These calls take no system or lane argument — they read the ambient
//! [execution context](crate::context). On this host transport the SDK is the
//! lane-id authority, so `create` hands out a real lane id synchronously, and a
//! submitted future runs on exactly that id (so its `Timer`/`Channel` calls see
//! the same id `lane.id()` reports). Completion is signalled over a one-slot
//! channel that `join` waits on with Catch-and-Release — no busy-waiting.

use crate::context::current_lane;
use crate::system::System;
use cibos_kernel::Channel;
use shared::protocols::ipc::{ChannelDirection, ChannelTerms};
use shared::{LaneId, WeightClass};
use std::future::Future;

/// Errors returned by lane operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneError {
    /// The container is already running its maximum number of lanes.
    ContainerAtCapacity,
    /// The system-wide lane limit is reached.
    SystemAtCapacity,
    /// A requested lane weight was outside the valid `1..=100` range.
    WeightOutOfRange,
    /// `submit` was called while a future is still running on this lane.
    AlreadyOccupied,
    /// An operation was attempted on a lane that has been destroyed.
    LaneDestroyed,
    /// `destroy` was called while a future is still running on this lane.
    LaneRunning,
}

/// A unit of concurrency within the container. Create one, submit work to it,
/// and join to await completion. Reusable across submit/join cycles.
pub struct Lane {
    system: System,
    id: LaneId,
    completion: Channel,
    occupied: bool,
    released: bool,
}

impl Lane {
    /// Reserve a new lane, returning a handle to it. The lane's id is assigned
    /// immediately and is available from [`Lane::id`] before any work is
    /// submitted.
    ///
    /// # Errors
    ///
    /// [`LaneError::ContainerAtCapacity`] if the container already holds its
    /// maximum number of lanes.
    ///
    /// # Panics
    ///
    /// Panics if called outside a running application (no ambient system).
    pub fn create() -> Result<Lane, LaneError> {
        let system = crate::context::current_system();
        let id = system
            .try_reserve_lane()
            .ok_or(LaneError::ContainerAtCapacity)?;
        Ok(Lane::from_reserved(system, id))
    }

    /// Build a handle around an already-reserved lane id, opening its one-slot
    /// completion channel.
    fn from_reserved(system: System, id: LaneId) -> Lane {
        let terms = ChannelTerms::new("lane-join", ChannelDirection::Bidirectional, 1, 1)
            .expect("lane-join channel terms are valid");
        let completion = system.open_channel(&terms);
        Lane {
            system,
            id,
            completion,
            occupied: false,
            released: false,
        }
    }

    /// Reserve a new lane with an explicit initial scheduling weight (`1..=100`).
    ///
    /// The weight is applied by the kernel only under a profile that supports
    /// per-lane weights (Compute); on other profiles the lane keeps its class
    /// weight, and the handle is created regardless.
    ///
    /// # Errors
    ///
    /// [`LaneError::WeightOutOfRange`] if `weight` is `0` or greater than `100`;
    /// [`LaneError::ContainerAtCapacity`] if the container is at its lane limit.
    ///
    /// # Panics
    ///
    /// Panics if called outside a running application (no ambient system).
    #[cfg(feature = "per-lane-weights")]
    pub fn create_with_weight(weight: u32) -> Result<Lane, LaneError> {
        if !(1..=100).contains(&weight) {
            return Err(LaneError::WeightOutOfRange);
        }
        let system = crate::context::current_system();
        let id = system
            .try_reserve_lane()
            .ok_or(LaneError::ContainerAtCapacity)?;
        let _ = system.set_lane_weight(id, weight);
        Ok(Lane::from_reserved(system, id))
    }

    /// This lane's identifier, unique within the container for the lane's
    /// lifetime. Useful for logging and correlating activity.
    #[must_use]
    pub fn id(&self) -> LaneId {
        self.id
    }

    /// Update this lane's scheduling weight at runtime (`1..=100`).
    ///
    /// Applied by the kernel only under a profile that supports dynamic weights
    /// (Compute); a no-op elsewhere. Takes effect on the next dispatch cycle.
    ///
    /// # Errors
    ///
    /// [`LaneError::WeightOutOfRange`] if `weight` is `0` or greater than `100`.
    #[cfg(feature = "dynamic-weights")]
    pub fn update_weight(&mut self, weight: u32) -> Result<(), LaneError> {
        if !(1..=100).contains(&weight) {
            return Err(LaneError::WeightOutOfRange);
        }
        let _ = self.system.update_lane_weight(self.id, weight);
        Ok(())
    }

    /// Submit a future to run on this lane. Only one future may run at a time;
    /// call [`Lane::join`] before submitting again.
    ///
    /// # Errors
    ///
    /// [`LaneError::AlreadyOccupied`] if a future is already running on this
    /// lane (it has not been joined).
    pub fn submit<F>(&mut self, future: F) -> Result<(), LaneError>
    where
        F: Future<Output = ()> + 'static,
    {
        if self.occupied {
            return Err(LaneError::AlreadyOccupied);
        }
        let completion = self.completion.clone();
        let id = self.id;
        // The wrapper signals completion after the user future finishes, so a
        // joiner stalled on the completion channel is released.
        self.system.spawn_on_reserved(WeightClass::User, id, async move {
            future.await;
            let _ = completion.send(id, vec![1u8]).await;
        });
        self.occupied = true;
        Ok(())
    }

    /// Wait for the lane's running future to finish. Returns immediately if no
    /// future is running. After it returns, the lane is free to accept another
    /// [`Lane::submit`].
    ///
    /// # Panics
    ///
    /// Panics if called outside a lane task (no ambient lane).
    pub async fn join(&mut self) {
        if !self.occupied {
            return;
        }
        let _ = self.completion.recv(current_lane()).await;
        self.occupied = false;
    }

    /// Destroy the lane, releasing its slot.
    ///
    /// # Errors
    ///
    /// [`LaneError::LaneRunning`] if a future is still running on this lane;
    /// the handle is consumed regardless, so join before destroying to be sure
    /// the work finished.
    pub fn destroy(mut self) -> Result<(), LaneError> {
        if self.occupied {
            // `self` drops here, releasing the slot via `Drop`.
            return Err(LaneError::LaneRunning);
        }
        self.system.release_reserved_lane();
        self.released = true;
        Ok(())
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        if !self.released {
            self.system.release_reserved_lane();
        }
    }
}
