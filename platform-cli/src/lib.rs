//! # `platform-cli` — CIBOS CLI Platform
//!
//! The platform layer for command-line applications. It provides the I/O
//! services the SDK does not — a line-oriented [`Console`] — and a [`CliRunner`]
//! that hosts a [`CliApp`] on top of the SDK's [`AppHost`].
//!
//! Two console backends ship here: [`StdConsole`], which uses the host's
//! standard input and output (the development path), and [`CaptureConsole`],
//! which records output and replays scripted input (the testing path). A CIBOS
//! display/TTY-backed console is a later addition; applications written against
//! the [`Console`] trait do not change.
//!
//! This mirrors the SDK's design: a real platform API surface, with a host
//! transport now and the on-device transport later.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use cibos_sdk::{AppHost, CibosProfile, ResourceLimits, System};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// The line-oriented console seam. Re-exported from the `cibos-console` crate so
/// that applications written against it (and `login::run_login`) run unchanged
/// whether the backend is the host [`StdConsole`] below or the on-kernel
/// syscall-backed console. The host backends [`StdConsole`] and
/// [`CaptureConsole`] implement this trait.
pub use cibos_console::Console;

/// A console backed by the host's standard input and output.
pub struct StdConsole;

impl Console for StdConsole {
    fn write_line(&self, line: &str) {
        println!("{line}");
    }

    fn read_line(&self) -> Option<String> {
        use std::io::BufRead;
        let mut buf = String::new();
        match std::io::stdin().lock().read_line(&mut buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(buf.trim_end_matches(['\n', '\r']).to_string()),
        }
    }
}

/// A console that captures output and replays scripted input. For tests.
pub struct CaptureConsole {
    output: Mutex<Vec<String>>,
    input: Mutex<VecDeque<String>>,
}

impl CaptureConsole {
    /// Create a capture console seeded with the given input lines.
    #[must_use]
    pub fn new(input: impl IntoIterator<Item = String>) -> Self {
        CaptureConsole {
            output: Mutex::new(Vec::new()),
            input: Mutex::new(input.into_iter().collect()),
        }
    }

    /// All lines written so far.
    #[must_use]
    pub fn output(&self) -> Vec<String> {
        self.output.lock().unwrap().clone()
    }

    /// The captured output joined with newlines.
    #[must_use]
    pub fn output_text(&self) -> String {
        self.output().join("\n")
    }
}

impl Console for CaptureConsole {
    fn write_line(&self, line: &str) {
        self.output.lock().unwrap().push(line.to_string());
    }

    fn read_line(&self) -> Option<String> {
        self.input.lock().unwrap().pop_front()
    }
}

/// The context a CLI application receives: the system handle plus the console.
#[derive(Clone)]
pub struct CliContext {
    /// The CIBOS system handle (spawn tasks, open channels, query limits).
    pub system: System,
    /// The console for I/O.
    pub console: Arc<dyn Console>,
}

/// A command-line application.
pub trait CliApp {
    /// A short human-readable name.
    fn name(&self) -> &str;

    /// Run the application. It spawns its tasks through `ctx.system` and does
    /// I/O through `ctx.console`; it returns promptly while the spawned tasks
    /// carry out the work.
    fn run(&self, ctx: CliContext);
}

/// Hosts a [`CliApp`] on an in-process kernel via the SDK.
pub struct CliRunner {
    host: AppHost,
    console: Arc<dyn Console>,
}

impl CliRunner {
    /// Create a runner with sensible defaults (2 execution contexts, Balanced
    /// profile, default application limits) over the given console.
    #[must_use]
    pub fn new(console: Arc<dyn Console>) -> Self {
        Self::with_config(
            console,
            2,
            [0x5Au8; 32],
            CibosProfile::Balanced,
            256,
            ResourceLimits::default_application(),
        )
    }

    /// Create a runner with explicit kernel configuration.
    #[must_use]
    pub fn with_config(
        console: Arc<dyn Console>,
        execution_contexts: usize,
        seed: [u8; 32],
        profile: CibosProfile,
        max_lanes: usize,
        limits: ResourceLimits,
    ) -> Self {
        let host = AppHost::new(execution_contexts, seed, profile, max_lanes, limits);
        CliRunner { host, console }
    }

    /// Run `app` to completion, returning the number of lane polls performed.
    pub fn run(&mut self, app: &dyn CliApp) -> usize {
        let ctx = CliContext {
            system: self.host.system(),
            console: self.console.clone(),
        };
        app.run(ctx);
        self.host.run()
    }

    /// Advance the kernel clock and continue running (for timer-driven apps).
    pub fn advance_and_run(&mut self, delta: std::time::Duration) -> usize {
        self.host.advance_and_run(delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cibos_sdk::WeightClass;

    struct EchoApp;
    impl CliApp for EchoApp {
        fn name(&self) -> &str {
            "echo"
        }
        fn run(&self, ctx: CliContext) {
            let console = ctx.console.clone();
            ctx.system.spawn(WeightClass::User, async move {
                while let Some(line) = console.read_line() {
                    console.write_line(&format!("echo: {line}"));
                }
            });
        }
    }

    #[test]
    fn cli_app_reads_and_writes_through_console() {
        let console = Arc::new(CaptureConsole::new(
            ["hello", "world"].iter().map(|s| s.to_string()),
        ));
        let mut runner = CliRunner::new(console.clone());
        runner.run(&EchoApp);
        assert_eq!(console.output(), alloc_vec(["echo: hello", "echo: world"]));
    }

    fn alloc_vec(items: impl IntoIterator<Item = &'static str>) -> Vec<String> {
        items.into_iter().map(|s| s.to_string()).collect()
    }
}
