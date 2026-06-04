//! Time and timer primitives (API Reference, Chapter 3).
//!
//! These are the documented free-function timer APIs. They take no system or
//! lane argument: they read the ambient [execution context](crate::context),
//! which the host runner installs while a lane is polled. Under the hood a sleep
//! is a Catch-and-Release stall on a kernel timer — the lane moves to the
//! Stalled List and the kernel returns it to the Ready Pool when the deadline
//! passes. No busy-waiting.

use crate::context::{current_lane, current_system};
use core::future::Future;
use core::task::{Context, Poll};
use std::time::Duration;

/// A monotonic point in time. Monotonic means it never goes backwards; it is not
/// wall-clock time. This is the instant type the documented API refers to.
pub type Instant = shared::types::time::Monotonic;

/// The current monotonic time.
///
/// # Panics
///
/// Panics if called outside a running application (no ambient system).
#[must_use]
pub fn now() -> Instant {
    current_system().now()
}

/// Async time primitives. All methods stall the current lane via the kernel
/// timer rather than blocking a thread.
pub struct Timer;

impl Timer {
    /// Stall the current lane for `duration`. A zero duration returns
    /// immediately. The lane resumes after at least `duration` has elapsed.
    ///
    /// # Panics
    ///
    /// Panics if called outside a lane task (no ambient lane).
    pub async fn sleep(duration: Duration) {
        let system = current_system();
        let lane = current_lane();
        system.sleep(lane, duration).await;
    }

    /// Stall the current lane until `instant`. If `instant` is already in the
    /// past, returns immediately.
    ///
    /// # Panics
    ///
    /// Panics if called outside a lane task (no ambient lane).
    pub async fn at(instant: Instant) {
        let remaining = instant.saturating_duration_since(now());
        Timer::sleep(remaining).await;
    }
}

/// Returned by [`with_timeout`] when the deadline elapses before the future
/// completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutError;

/// Run `future` with a timeout. Returns `Ok(value)` if the future completes
/// within `duration`, or `Err(TimeoutError)` if the timeout elapses first. If
/// both are ready on the same poll, the future's result is preferred.
///
/// # Panics
///
/// Panics if called outside a lane task (no ambient lane).
pub async fn with_timeout<T, F>(duration: Duration, future: F) -> Result<T, TimeoutError>
where
    F: Future<Output = T>,
{
    let mut future = Box::pin(future);
    let mut timer = Box::pin(Timer::sleep(duration));
    core::future::poll_fn(move |cx: &mut Context<'_>| {
        if let Poll::Ready(value) = future.as_mut().poll(cx) {
            return Poll::Ready(Ok(value));
        }
        if timer.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(TimeoutError));
        }
        Poll::Pending
    })
    .await
}
