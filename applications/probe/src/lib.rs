//! # Probe
//!
//! A Gate scanner and Warden (firewall) tool for the CIBOS Lattice. It binds a
//! couple of demonstration service Gates, then answers commands:
//!
//! * `scan <start> <end>` — report each Gate in the range as open, closed, or
//!   blocked (firewalled).
//! * `gates` — list every open Gate.
//! * `block <gate>` / `allow <gate>` — adjust the Warden, then a rescan shows
//!   the effect.
//!
//! It demonstrates the Lattice end to end: binding Gates, the Warden denying
//! access, and scanning — all over the in-memory fabric.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use cibos_sdk::{Lattice, WeightClass};
use platform_cli::{CliApp, CliContext, Console};

/// Handle one probe command against the fabric.
pub fn handle(net: &Lattice, line: &str, console: &dyn Console) {
    let mut parts = line.split_whitespace();
    let Some(cmd) = parts.next() else {
        return;
    };
    match cmd {
        "scan" => {
            let start = parts.next().and_then(|s| s.parse::<u16>().ok());
            let end = parts.next().and_then(|s| s.parse::<u16>().ok());
            let (Some(start), Some(end)) = (start, end) else {
                console.write_line("usage: scan <start> <end>");
                return;
            };
            if end < start {
                console.write_line("end must be >= start");
                return;
            }
            for status in net.scan(start..=end) {
                let state = if status.blocked {
                    "blocked (firewall)"
                } else if status.open {
                    "open"
                } else {
                    "closed"
                };
                console.write_line(&format!("gate {}: {state}", status.gate));
            }
        }
        "gates" => {
            let open = net.open_gates();
            if open.is_empty() {
                console.write_line("(no open gates)");
            } else {
                let list: Vec<String> = open.iter().map(u16::to_string).collect();
                console.write_line(&format!("open gates: {}", list.join(" ")));
            }
        }
        "block" => match parts.next().and_then(|s| s.parse::<u16>().ok()) {
            Some(g) => {
                net.warden_deny(g);
                console.write_line(&format!("warden: blocked gate {g}"));
            }
            None => console.write_line("usage: block <gate>"),
        },
        "allow" => match parts.next().and_then(|s| s.parse::<u16>().ok()) {
            Some(g) => {
                net.warden_allow(g);
                console.write_line(&format!("warden: allowed gate {g}"));
            }
            None => console.write_line("usage: allow <gate>"),
        },
        other => console.write_line(&format!("unknown probe command: {other}")),
    }
}

/// The probe application.
#[derive(Default)]
pub struct Probe;

impl Probe {
    /// Create the probe app.
    #[must_use]
    pub fn new() -> Self {
        Probe
    }
}

impl CliApp for Probe {
    fn name(&self) -> &str {
        "probe"
    }

    fn run(&self, ctx: CliContext) {
        let net = ctx.system.lattice();
        let console = ctx.console.clone();

        // Stand up a couple of demonstration services and firewall one Gate.
        let mut listeners = Vec::new();
        for gate in [80u16, 443] {
            if let Ok(l) = net.bind(gate) {
                listeners.push(l);
            }
        }
        net.warden_deny(23);

        ctx.system.spawn(WeightClass::User, async move {
            // Hold the listeners for the session so the Gates stay open.
            let _services = listeners;
            while let Some(line) = console.read_line() {
                handle(&net, &line, &*console);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_cli::{CaptureConsole, CliRunner};
    use std::sync::Arc;

    fn run(input: &[&str]) -> Vec<String> {
        let console = Arc::new(CaptureConsole::new(input.iter().map(|s| s.to_string())));
        let mut runner = CliRunner::new(console.clone());
        runner.run(&Probe::new());
        console.output()
    }

    #[test]
    fn scan_shows_open_closed_blocked() {
        let out = run(&["scan 79 81", "scan 23 23", "gates"]).join("\n");
        assert!(out.contains("gate 80: open"));
        assert!(out.contains("gate 79: closed"));
        assert!(out.contains("gate 81: closed"));
        assert!(out.contains("gate 23: blocked (firewall)"));
        assert!(out.contains("open gates: 80 443"));
    }

    #[test]
    fn warden_block_then_rescan() {
        let out = run(&["block 80", "scan 80 80", "allow 80", "scan 80 80"]);
        let text = out.join("\n");
        assert!(text.contains("warden: blocked gate 80"));
        // After blocking, 80 is firewalled even though a listener is bound.
        assert!(text.contains("gate 80: blocked (firewall)"));
        // After allowing, it's open again.
        assert!(text.contains("warden: allowed gate 80"));
        assert!(text.contains("gate 80: open"));
    }

    #[test]
    fn unknown_command() {
        let out = run(&["nmap-style-thing"]).join("\n");
        assert!(out.contains("unknown probe command"));
    }
}
