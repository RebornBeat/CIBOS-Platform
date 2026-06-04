//! # Applications and the Host Runner
//!
//! [`Application`] is the entry-point convention an application implements: its
//! [`Application::start`] is called once with a [`System`] handle, through which
//! it opens channels and spawns its initial tasks.
//!
//! [`AppHost`] runs an application against an in-process [`Kernel`]. This is the
//! development and test transport — it is how a CIBOS application is exercised
//! before there is hardware or a syscall boundary. The production transport
//! (the same [`System`] API backed by syscalls to a booted CIBOS kernel) is a
//! later addition; applications written against [`System`] do not change.

use crate::system::System;
use cibos_kernel::Kernel;
use shared::{CibosProfile, ResourceLimits, WeightClass};

/// The entry-point convention for a CIBOS application.
pub trait Application {
    /// A short human-readable name.
    fn name(&self) -> &str;

    /// Called once at startup. The application opens channels and spawns its
    /// initial tasks through `system`; it returns promptly, and the spawned
    /// tasks carry out the work.
    fn start(&self, system: System);
}

/// Runs an [`Application`] against an in-process kernel.
pub struct AppHost {
    kernel: Kernel,
    system: System,
}

impl AppHost {
    /// Create a host with `execution_contexts` contexts, seeded from `seed`,
    /// under `profile`, allowing up to `max_lanes` lanes, granting the
    /// application `limits`.
    #[must_use]
    pub fn new(
        execution_contexts: usize,
        seed: [u8; 32],
        profile: CibosProfile,
        max_lanes: usize,
        limits: ResourceLimits,
    ) -> Self {
        let kernel = Kernel::new(execution_contexts, seed, profile, max_lanes);
        // Register this application as a user container in the kernel's isolation
        // registry; its `BoundaryId` is the application's container id.
        let boundary = kernel.containers().create(limits, WeightClass::User);
        let system = System::new(
            kernel.interface(),
            limits,
            boundary,
            crate::system::IdSource::new(),
            crate::broker::ChannelBroker::new(),
        );
        AppHost { kernel, system }
    }

    /// A clone of the application's system handle.
    #[must_use]
    pub fn system(&self) -> System {
        self.system.clone()
    }

    /// Launch `app` and drive it to completion. Returns the total number of
    /// lane polls performed (useful for tests and instrumentation).
    pub fn launch(&mut self, app: &dyn Application) -> usize {
        let _guard = crate::context::SystemGuard::enter(self.system.clone());
        app.start(self.system.clone());
        self.run_inner()
    }

    /// Drive the spawn/schedule loop until the application is idle: install any
    /// queued spawns, run the scheduler until no lane is ready, and repeat while
    /// new spawns keep appearing.
    pub fn run(&mut self) -> usize {
        let _guard = crate::context::SystemGuard::enter(self.system.clone());
        self.run_inner()
    }

    /// The spawn/schedule loop itself. Callers install the ambient system first
    /// (see [`AppHost::run`] / [`AppHost::launch`]); the documented free-function
    /// API reads it from that context while tasks are polled here.
    fn run_inner(&mut self) -> usize {
        let mut total_polls = 0usize;
        loop {
            // Install queued spawns into the kernel at their minted lane ids.
            for spawn in self.system.take_pending() {
                if self
                    .kernel
                    .spawn_on(spawn.lane, spawn.class, spawn.future)
                    .is_err()
                {
                    // Executor rejected it (at capacity); undo the slot count.
                    self.system.note_lane_complete();
                }
            }

            // Run all currently-runnable lanes to a stall point.
            total_polls += self.kernel.run_until_idle();

            // Done when nothing is queued and nothing remains scheduled. If the
            // only thing left is lanes parked on timers, jump the clock to the
            // next deadline to release them and keep running; a genuinely idle
            // system (no ready, no pending, no timer waits) ends the loop.
            if self.system.pending_is_empty() && !self.kernel.scheduler().has_ready() {
                if self.kernel.advance_to_next_timer() {
                    continue;
                }
                break;
            }
        }
        total_polls
    }

    /// Borrow the underlying kernel (for advancing the clock, inspection, etc.).
    #[must_use]
    pub fn kernel(&self) -> &Kernel {
        &self.kernel
    }

    /// Advance the kernel clock and continue running (releases matured timers).
    pub fn advance_and_run(&mut self, delta: core::time::Duration) -> usize {
        self.kernel.advance_clock(delta);
        self.run()
    }
}
