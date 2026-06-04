//! # Time
//!
//! A `no_std` monotonic clock representation.
//!
//! `std::time::Instant` is unavailable in firmware and kernel, so the system
//! uses [`Monotonic`] — a monotonic point in time measured in nanoseconds since
//! an arbitrary epoch (in practice, since boot). The kernel produces these from
//! its hardware timer; everything above consumes them.
//!
//! Durations use the standard [`core::time::Duration`], which is already
//! `no_std`. This module only adds the monotonic *instant* that `core` lacks.

use core::time::Duration;

/// A monotonic point in time, in nanoseconds since an unspecified epoch.
///
/// Monotonic means it never moves backwards. It is **not** wall-clock time and
/// must not be interpreted as a calendar date. Comparisons and differences
/// between two `Monotonic` values are meaningful; the absolute value is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Monotonic {
    nanos: u64,
}

impl Monotonic {
    /// The zero point (epoch).
    pub const ZERO: Monotonic = Monotonic { nanos: 0 };

    /// Construct from a raw nanosecond count since the epoch.
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Monotonic { nanos }
    }

    /// Construct from a millisecond count since the epoch.
    #[must_use]
    pub const fn from_millis(millis: u64) -> Self {
        Monotonic {
            nanos: millis.saturating_mul(1_000_000),
        }
    }

    /// The raw nanosecond count since the epoch.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.nanos
    }

    /// The duration elapsed from `earlier` to `self`, saturating at zero if
    /// `earlier` is later than `self`.
    #[must_use]
    pub fn saturating_duration_since(self, earlier: Monotonic) -> Duration {
        Duration::from_nanos(self.nanos.saturating_sub(earlier.nanos))
    }

    /// `self + duration`, or `None` on overflow.
    #[must_use]
    pub fn checked_add(self, duration: Duration) -> Option<Monotonic> {
        let add = u64::try_from(duration.as_nanos()).ok()?;
        self.nanos.checked_add(add).map(|nanos| Monotonic { nanos })
    }

    /// `self + duration`, saturating at the maximum on overflow.
    #[must_use]
    pub fn saturating_add(self, duration: Duration) -> Monotonic {
        let add = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        Monotonic {
            nanos: self.nanos.saturating_add(add),
        }
    }

    /// Whether `self` is at or after `deadline`.
    #[must_use]
    pub const fn reached(self, deadline: Monotonic) -> bool {
        self.nanos >= deadline.nanos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_since_saturates() {
        let a = Monotonic::from_millis(100);
        let b = Monotonic::from_millis(40);
        assert_eq!(a.saturating_duration_since(b), Duration::from_millis(60));
        // Reversed: saturates to zero rather than underflowing.
        assert_eq!(b.saturating_duration_since(a), Duration::from_millis(0));
    }

    #[test]
    fn add_and_reached() {
        let start = Monotonic::from_millis(1000);
        let deadline = start.checked_add(Duration::from_millis(50)).unwrap();
        assert!(!start.reached(deadline));
        assert!(deadline.reached(deadline));
        assert!(Monotonic::from_millis(1051).reached(deadline));
    }

    #[test]
    fn saturating_add_caps() {
        let near_max = Monotonic::from_nanos(u64::MAX - 5);
        let capped = near_max.saturating_add(Duration::from_secs(1));
        assert_eq!(capped.as_nanos(), u64::MAX);
    }
}
