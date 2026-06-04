//! # `cibos-async-runtime` — HIP-Native Async Runtime
//!
//! The async executor CIBOS runs lanes on. Unlike a conventional runtime it has
//! no scheduling loop of its own: the kernel is the scheduler, and this crate
//! provides the mechanism it drives.
//!
//! ## The Catch-and-Release cycle
//!
//! 1. A lane (a future) is spawned into the [`LaneExecutor`]; spawning requests
//!    its first poll through [`shared::KernelInterface::signal_ready`].
//! 2. The kernel selector picks a ready lane and calls
//!    [`LaneExecutor::poll_lane`].
//! 3. If the future cannot progress it awaits a [`ResourceWait`], which calls
//!    [`shared::KernelInterface::register_wait`] and returns `Pending`. The lane
//!    moves to the kernel's Stalled List and consumes no cycles ("catch").
//! 4. When the kernel sees the resource is available it signals the lane ready;
//!    the lane is re-polled and proceeds ("release").
//!
//! The waker ([`CibosWaker`]) is the only path from `Pending` back to runnable,
//! and it routes through the kernel — so all scheduling policy stays in the
//! kernel, while this crate stays a small, verifiable mechanism.
//!
//! ## `no_std`
//!
//! `no_std` with `alloc` (futures are boxed). No global allocator is defined
//! here — the kernel provides one. `std` is enabled only for the host tests.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

pub mod error;
pub mod executor;
pub mod future;
pub mod waker;

pub use error::{RuntimeError, RuntimeResult};
pub use executor::{LaneExecutor, LanePoll};
pub use future::{yield_now, ResourceWait, YieldNow};
pub use waker::CibosWaker;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use shared::protocols::ipc::WaitResource;
    use shared::types::time::Monotonic;
    use shared::{KernelInterface, LaneId};
    use std::sync::Mutex;

    /// A minimal in-test kernel: a ready queue, a record of registered waits,
    /// and a clock. Stands in for the CIBOS scheduler so the runtime mechanism
    /// can be exercised end-to-end.
    struct TestKernel {
        state: Mutex<TestState>,
    }

    struct TestState {
        ready: VecDeque<LaneId>,
        waits: Vec<(LaneId, WaitResource)>,
        now_nanos: u64,
    }

    impl TestKernel {
        fn new() -> Arc<Self> {
            Arc::new(TestKernel {
                state: Mutex::new(TestState {
                    ready: VecDeque::new(),
                    waits: Vec::new(),
                    now_nanos: 0,
                }),
            })
        }
        fn next_ready(&self) -> Option<LaneId> {
            self.state.lock().unwrap().ready.pop_front()
        }
        fn wait_count(&self) -> usize {
            self.state.lock().unwrap().waits.len()
        }
    }

    impl KernelInterface for TestKernel {
        fn register_wait(&self, lane: LaneId, resource: WaitResource) {
            self.state.lock().unwrap().waits.push((lane, resource));
        }
        fn signal_ready(&self, lane: LaneId) {
            self.state.lock().unwrap().ready.push_back(lane);
        }
        fn now(&self) -> Monotonic {
            Monotonic::from_nanos(self.state.lock().unwrap().now_nanos)
        }
    }

    /// Drive the executor until no lane is ready, returning the number of polls.
    fn drain(exec: &mut LaneExecutor, kernel: &Arc<TestKernel>) -> usize {
        let mut polls = 0;
        while let Some(lane) = kernel.next_ready() {
            exec.poll_lane(lane);
            polls += 1;
            assert!(polls < 100_000, "runaway scheduling loop");
        }
        polls
    }

    fn dyn_kernel(k: &Arc<TestKernel>) -> Arc<dyn KernelInterface> {
        k.clone()
    }

    #[test]
    fn completes_simple_future() {
        let kernel = TestKernel::new();
        let mut exec = LaneExecutor::new(dyn_kernel(&kernel), 64);

        let flag = Arc::new(AtomicUsize::new(0));
        let f = flag.clone();
        let lane = exec
            .spawn(async move {
                f.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();

        assert!(exec.has_lane(lane));
        drain(&mut exec, &kernel);

        assert_eq!(flag.load(Ordering::SeqCst), 1);
        assert_eq!(exec.lane_count(), 0, "completed lane should be reaped");
    }

    #[test]
    fn yield_now_reschedules_then_completes() {
        let kernel = TestKernel::new();
        let mut exec = LaneExecutor::new(dyn_kernel(&kernel), 64);

        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        exec.spawn(async move {
            c.fetch_add(1, Ordering::SeqCst);
            yield_now().await;
            c.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

        let polls = drain(&mut exec, &kernel);
        // One poll runs to the yield; the yield re-signals readiness; a second
        // poll completes the future.
        assert_eq!(polls, 2);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        assert_eq!(exec.lane_count(), 0);
    }

    #[test]
    fn resource_wait_parks_then_releases() {
        let kernel = TestKernel::new();
        let mut exec = LaneExecutor::new(dyn_kernel(&kernel), 64);

        let done = Arc::new(AtomicUsize::new(0));
        let d = done.clone();
        let kresource = dyn_kernel(&kernel);
        let lane = exec
            .spawn_with_lane(move |lane| async move {
                // Stall on a timer resource until the kernel releases us.
                ResourceWait::new(kresource, lane, WaitResource::Timer(Monotonic::from_millis(5)))
                    .await;
                d.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();

        // First drive: the lane registers its wait and parks (no completion).
        drain(&mut exec, &kernel);
        assert_eq!(done.load(Ordering::SeqCst), 0, "should still be parked");
        assert_eq!(kernel.wait_count(), 1, "wait should be registered once");
        assert!(exec.has_lane(lane), "lane stays in flight while stalled");

        // The kernel observes the resource is ready and releases the lane.
        kernel.signal_ready(lane);
        drain(&mut exec, &kernel);

        assert_eq!(done.load(Ordering::SeqCst), 1, "lane completes after release");
        assert_eq!(exec.lane_count(), 0);
    }

    #[test]
    fn lane_limit_is_enforced() {
        let kernel = TestKernel::new();
        let mut exec = LaneExecutor::new(dyn_kernel(&kernel), 1);

        // A lane that parks forever (never signalled ready again).
        let kresource = dyn_kernel(&kernel);
        exec.spawn_with_lane(move |lane| async move {
            ResourceWait::new(kresource, lane, WaitResource::Memory(1024)).await;
        })
        .unwrap();

        // The executor is at capacity; a second spawn must be rejected.
        let second = exec.spawn(async {});
        assert!(matches!(
            second,
            Err(RuntimeError::LaneLimitExceeded { limit: 1 })
        ));
    }

    #[test]
    fn abort_removes_lane() {
        let kernel = TestKernel::new();
        let mut exec = LaneExecutor::new(dyn_kernel(&kernel), 64);
        let kresource = dyn_kernel(&kernel);
        let lane = exec
            .spawn_with_lane(move |lane| async move {
                ResourceWait::new(kresource, lane, WaitResource::Memory(1)).await;
            })
            .unwrap();
        drain(&mut exec, &kernel);
        assert!(exec.has_lane(lane));
        assert!(exec.abort(lane));
        assert!(!exec.has_lane(lane));
        assert!(!exec.abort(lane), "double abort is a no-op");
    }
}
