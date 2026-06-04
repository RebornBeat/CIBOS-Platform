//! # Server platform
//!
//! A headless host for long-running services — no console, no display, just a
//! [`System`] (channels, spawning, timers, filesystem, the Lattice). It runs an
//! [`Application`]'s tasks to quiescence, and exposes the system afterward so a
//! caller (or test) can inspect what the service produced.
//!
//! This is the platform a daemon like Vane runs under in production: bind a
//! Gate, spawn worker lanes, serve. Here the runner drives the same scheduler
//! the other platforms use, without any UI.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use cibos_sdk::{AppHost, Application, CibosProfile, ResourceLimits, System};
use std::time::Duration;

/// A headless runner for server applications.
pub struct ServerRunner {
    host: AppHost,
}

impl Default for ServerRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerRunner {
    /// Create a server runner with default configuration (two execution
    /// contexts, balanced profile, default application limits).
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(
            2,
            [0x5Au8; 32],
            CibosProfile::Balanced,
            256,
            ResourceLimits::default_application(),
        )
    }

    /// Create a server runner with explicit kernel configuration.
    #[must_use]
    pub fn with_config(
        execution_contexts: usize,
        seed: [u8; 32],
        profile: CibosProfile,
        max_lanes: usize,
        limits: ResourceLimits,
    ) -> Self {
        ServerRunner {
            host: AppHost::new(execution_contexts, seed, profile, max_lanes, limits),
        }
    }

    /// The system handle (filesystem, Lattice, etc.) — useful for seeding state
    /// before `run` and inspecting results after.
    #[must_use]
    pub fn system(&self) -> System {
        self.host.system()
    }

    /// Start the application and drive its tasks to quiescence. Returns the
    /// number of lane polls performed.
    pub fn run(&mut self, app: &dyn Application) -> usize {
        self.host.launch(app)
    }

    /// Advance the kernel clock and continue running (releases matured timers).
    pub fn advance_and_run(&mut self, delta: Duration) -> usize {
        self.host.advance_and_run(delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cibos_sdk::WeightClass;

    /// A headless service: on start it spawns a worker that reads numbers from
    /// the filesystem, sums them, and writes the total back — then completes.
    struct SummingService;

    impl Application for SummingService {
        fn name(&self) -> &str {
            "summing-service"
        }
        fn start(&self, system: System) {
            let fs = system.filesystem();
            system.spawn(WeightClass::Background, async move {
                let input = fs.read("/in/numbers").unwrap_or_default();
                let text = String::from_utf8_lossy(&input);
                let sum: i64 = text.split_whitespace().filter_map(|t| t.parse::<i64>().ok()).sum();
                fs.write("/out/total", sum.to_string().as_bytes());
            });
        }
    }

    #[test]
    fn hosts_a_headless_service() {
        let mut runner = ServerRunner::new();
        // Seed input through the shared filesystem.
        runner.system().filesystem().write("/in/numbers", b"3 4 5 6");
        runner.run(&SummingService);
        // The service produced its output.
        let out = runner.system().filesystem().read("/out/total").unwrap();
        assert_eq!(String::from_utf8_lossy(&out), "18");
    }

    #[test]
    fn system_is_shared_across_calls() {
        let runner = ServerRunner::new();
        runner.system().filesystem().write("/k", b"v");
        // A fresh system handle observes the same store.
        assert_eq!(
            runner.system().filesystem().read("/k").as_deref(),
            Some(&b"v"[..])
        );
    }
}
