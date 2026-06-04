//! Ambient execution context for the documented free-function API.
//!
//! The documented application API (`Timer::sleep`, `Lane::create`,
//! `container::*`, `cibos::time::now`) takes no `System` handle and no
//! [`LaneId`] argument — it reads them from the current execution context. On
//! this host transport the context is task/thread-local: the runner is
//! single-threaded and polls one lane at a time, so this is lock-free,
//! single-owner state (no `Mutex`, consistent with ADR-001). On a booted kernel
//! the same ambient context is the per-lane syscall context.
//!
//! Two pieces make up the context:
//! - the [`System`], which is constant for an application run and is installed
//!   by the host runner via [`SystemGuard`] for the duration of a launch/run;
//! - the current [`LaneId`], which changes per dispatched lane and is installed
//!   per poll by [`WithLaneContext`], a future wrapper the runner builds around
//!   each lane task (the runner-assigned lane id is known at build time).

use crate::system::System;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use shared::LaneId;
use std::cell::RefCell;

thread_local! {
    static CURRENT_SYSTEM: RefCell<Option<System>> = const { RefCell::new(None) };
    static CURRENT_LANE: RefCell<Option<LaneId>> = const { RefCell::new(None) };
}

/// RAII guard that installs the ambient [`System`] for its lifetime, restoring
/// the previous value on drop. Installed by the host runner around a launch/run
/// so the documented API can reach the system without it being threaded through.
pub(crate) struct SystemGuard {
    previous: Option<System>,
}

impl SystemGuard {
    pub(crate) fn enter(system: System) -> Self {
        let previous = CURRENT_SYSTEM.with(|c| c.borrow_mut().replace(system));
        SystemGuard { previous }
    }
}

impl Drop for SystemGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT_SYSTEM.with(|c| *c.borrow_mut() = previous);
    }
}

/// The ambient system handle.
///
/// # Panics
///
/// Panics if called outside a running application (no host runner active). The
/// documented free-function API is only meaningful inside an application's
/// lanes; calling it elsewhere is a programming error, not a runtime condition.
#[must_use]
pub(crate) fn current_system() -> System {
    CURRENT_SYSTEM.with(|c| {
        c.borrow().clone().expect(
            "no CIBOS system in scope: this API is only valid inside a running application",
        )
    })
}

/// The ambient lane id.
///
/// # Panics
///
/// Panics if called outside a lane task (for example from a plain spawn that was
/// given no lane id). The documented lane-bound API must run on a lane.
#[must_use]
pub(crate) fn current_lane() -> LaneId {
    CURRENT_LANE.with(|c| {
        c.borrow()
            .expect("no CIBOS lane in scope: this API must be called from within a lane task")
    })
}

/// A future wrapper that installs the lane's execution context — its owning
/// [`System`] and its [`LaneId`] — as ambient for the duration of each poll,
/// restoring the previous values afterward. Both are known when the task is
/// built, so the documented free-function API resolves without the application
/// threading them through. Because the system is installed per poll, lanes
/// belonging to different containers each see their own system (the basis for
/// cross-container work); in a single-container run every lane installs the same
/// system, so this is transparent.
///
/// When `release_on_complete` is set, the wrapper also releases the system's
/// in-flight lane slot once the future finishes — used for fire-and-forget
/// spawns. Lanes held behind a [`Lane`](crate::Lane) handle set it to `false`
/// and release their slot on `destroy` instead, so the slot persists across
/// successive `submit`/`join` cycles.
pub(crate) struct WithLaneContext {
    lane: LaneId,
    system: System,
    release_on_complete: bool,
    inner: Pin<Box<dyn Future<Output = ()>>>,
}

impl WithLaneContext {
    pub(crate) fn new(
        lane: LaneId,
        system: System,
        release_on_complete: bool,
        inner: Pin<Box<dyn Future<Output = ()>>>,
    ) -> Self {
        WithLaneContext {
            lane,
            system,
            release_on_complete,
            inner,
        }
    }
}

impl Future for WithLaneContext {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // `WithLaneContext` is `Unpin` (a `LaneId` is `Copy`, `System` is an
        // `Rc` handle, and `Pin<Box<_>>` is `Unpin`), so this safe projection
        // needs no `unsafe`.
        let this = self.get_mut();
        let prev_lane = CURRENT_LANE.with(|c| c.borrow_mut().replace(this.lane));
        let prev_system = CURRENT_SYSTEM.with(|c| c.borrow_mut().replace(this.system.clone()));
        let result = this.inner.as_mut().poll(cx);
        CURRENT_SYSTEM.with(|c| *c.borrow_mut() = prev_system);
        CURRENT_LANE.with(|c| *c.borrow_mut() = prev_lane);
        if result.is_ready() && this.release_on_complete {
            this.system.note_lane_complete();
        }
        result
    }
}
