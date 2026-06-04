//! # Clock
//!
//! A stopwatch and lap timer. It operates on monotonic nanosecond readings
//! (obtain them from `System::now().as_nanos()`), keeping the logic pure and
//! testable without a running kernel: feed it timestamps and it reports elapsed
//! and lap durations.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Stopwatch state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Running { started_at: u64, last_lap_at: u64 },
    Stopped { elapsed: u64 },
}

/// A stopwatch over monotonic nanosecond timestamps.
#[derive(Debug, Clone)]
pub struct Stopwatch {
    state: State,
    laps: Vec<u64>,
}

impl Default for Stopwatch {
    fn default() -> Self {
        Self::new()
    }
}

impl Stopwatch {
    /// A fresh, idle stopwatch.
    #[must_use]
    pub fn new() -> Self {
        Stopwatch {
            state: State::Idle,
            laps: Vec::new(),
        }
    }

    /// Start (or restart) the stopwatch at `now` nanoseconds.
    pub fn start(&mut self, now: u64) {
        self.state = State::Running {
            started_at: now,
            last_lap_at: now,
        };
        self.laps.clear();
    }

    /// Whether the stopwatch is currently running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self.state, State::Running { .. })
    }

    /// Record a lap at `now`, returning the lap duration (since the previous
    /// lap or start) in nanoseconds. No-op returning 0 if not running.
    pub fn lap(&mut self, now: u64) -> u64 {
        if let State::Running { last_lap_at, .. } = &mut self.state {
            let lap = now.saturating_sub(*last_lap_at);
            *last_lap_at = now;
            self.laps.push(lap);
            lap
        } else {
            0
        }
    }

    /// Elapsed nanoseconds since start (frozen once stopped).
    #[must_use]
    pub fn elapsed(&self, now: u64) -> u64 {
        match self.state {
            State::Idle => 0,
            State::Running { started_at, .. } => now.saturating_sub(started_at),
            State::Stopped { elapsed } => elapsed,
        }
    }

    /// Stop at `now`, freezing the elapsed time. Returns total elapsed ns.
    pub fn stop(&mut self, now: u64) -> u64 {
        let elapsed = self.elapsed(now);
        self.state = State::Stopped { elapsed };
        elapsed
    }

    /// Reset to idle, clearing laps.
    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.laps.clear();
    }

    /// Recorded lap durations (nanoseconds).
    #[must_use]
    pub fn laps(&self) -> &[u64] {
        &self.laps
    }
}

/// Format a nanosecond duration as `s.mmm` seconds (milliseconds precision).
#[must_use]
pub fn format_seconds(nanos: u64) -> String {
    let ms = nanos / 1_000_000;
    format!("{}.{:03}s", ms / 1000, ms % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000; // ns per ms

    #[test]
    fn elapsed_tracks_running_then_freezes() {
        let mut sw = Stopwatch::new();
        assert_eq!(sw.elapsed(1000), 0); // idle
        sw.start(1000 * MS);
        assert!(sw.is_running());
        assert_eq!(sw.elapsed(1500 * MS), 500 * MS);
        let total = sw.stop(2000 * MS);
        assert_eq!(total, 1000 * MS);
        // Frozen after stop.
        assert_eq!(sw.elapsed(9999 * MS), 1000 * MS);
        assert!(!sw.is_running());
    }

    #[test]
    fn laps_measure_intervals() {
        let mut sw = Stopwatch::new();
        sw.start(0);
        assert_eq!(sw.lap(100 * MS), 100 * MS);
        assert_eq!(sw.lap(250 * MS), 150 * MS);
        assert_eq!(sw.lap(300 * MS), 50 * MS);
        assert_eq!(sw.laps(), &[100 * MS, 150 * MS, 50 * MS]);
    }

    #[test]
    fn lap_when_idle_is_noop() {
        let mut sw = Stopwatch::new();
        assert_eq!(sw.lap(100), 0);
        assert!(sw.laps().is_empty());
    }

    #[test]
    fn reset_clears() {
        let mut sw = Stopwatch::new();
        sw.start(0);
        sw.lap(10 * MS);
        sw.reset();
        assert_eq!(sw.elapsed(99), 0);
        assert!(sw.laps().is_empty());
    }

    #[test]
    fn formatting() {
        assert_eq!(format_seconds(1_500 * MS), "1.500s");
        assert_eq!(format_seconds(250 * MS), "0.250s");
        assert_eq!(format_seconds(0), "0.000s");
    }
}
