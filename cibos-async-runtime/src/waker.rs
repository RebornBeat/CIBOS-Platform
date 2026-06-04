//! # The Lane Waker
//!
//! [`CibosWaker`] is the bridge from a future's `Poll::Pending` back to the
//! kernel scheduler. When a future is woken — because the resource it stalled
//! on became available — the waker calls
//! [`KernelInterface::signal_ready`](shared::KernelInterface::signal_ready) with
//! the lane's identifier. The kernel then re-qualifies the lane and decides,
//! per Catch-and-Release, when to poll it again.
//!
//! The waker carries only a lane identifier and a reference-counted handle to
//! the kernel interface, so it is cheap to clone (each clone bumps the refcount)
//! and `Send + Sync`, which the standard [`core::task::Waker`] requires.

use alloc::sync::Arc;
use alloc::task::Wake;
use core::task::Waker;
use shared::{KernelInterface, LaneId};

/// A waker that signals a lane's readiness to the kernel.
pub struct CibosWaker {
    lane: LaneId,
    kernel: Arc<dyn KernelInterface>,
}

impl CibosWaker {
    /// Build a [`core::task::Waker`] for `lane` backed by `kernel`.
    #[must_use]
    pub fn waker_for(lane: LaneId, kernel: Arc<dyn KernelInterface>) -> Waker {
        Waker::from(Arc::new(CibosWaker { lane, kernel }))
    }
}

impl Wake for CibosWaker {
    fn wake(self: Arc<Self>) {
        self.kernel.signal_ready(self.lane);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.kernel.signal_ready(self.lane);
    }
}
