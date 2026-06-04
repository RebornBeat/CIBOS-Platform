//! # HIP-Native Futures
//!
//! Two foundational futures the rest of the system builds on.
//!
//! [`yield_now`] cooperatively yields the lane once, letting the scheduler run
//! other ready lanes before continuing. It re-signals readiness immediately, so
//! it never stalls on a resource.
//!
//! [`ResourceWait`] is the Catch-and-Release primitive. On its first poll it
//! registers the lane as waiting on a specific resource and returns
//! `Poll::Pending`; the lane then sits in the kernel's Stalled List, consuming
//! no cycles. When the kernel observes the resource is available it signals the
//! lane ready and re-polls, at which point the future completes. Resource-
//! specific futures in the kernel (channel receive, timer, I/O completion) are
//! thin wrappers that choose the right [`WaitResource`] and layer their own
//! readiness check on top of this primitive.

use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use shared::protocols::ipc::WaitResource;
use shared::{KernelInterface, LaneId};

/// Yield the current lane once. Returns `Pending` the first time (after asking
/// to be polled again immediately) and `Ready` thereafter.
#[must_use]
pub fn yield_now() -> YieldNow {
    YieldNow { yielded: false }
}

/// Future returned by [`yield_now`].
#[derive(Debug)]
pub struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            // Ask to be polled again on the next scheduling pass.
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

/// The Catch-and-Release wait primitive: stalls the lane on a [`WaitResource`]
/// until the kernel signals readiness and re-polls.
pub struct ResourceWait {
    kernel: Arc<dyn KernelInterface>,
    lane: LaneId,
    resource: WaitResource,
    registered: bool,
}

impl ResourceWait {
    /// Create a wait on `resource` for `lane`, signalling the kernel through
    /// `kernel`.
    #[must_use]
    pub fn new(kernel: Arc<dyn KernelInterface>, lane: LaneId, resource: WaitResource) -> Self {
        Self {
            kernel,
            lane,
            resource,
            registered: false,
        }
    }
}

impl Future for ResourceWait {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.registered {
            // We have been re-polled, which only happens after the kernel
            // signalled this lane ready: the resource is available.
            Poll::Ready(())
        } else {
            self.registered = true;
            let resource = self.resource;
            let lane = self.lane;
            self.kernel.register_wait(lane, resource);
            Poll::Pending
        }
    }
}
