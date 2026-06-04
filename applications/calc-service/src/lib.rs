//! # Calculator Service
//!
//! A demonstration of CIBOS's intended IPC model: two isolated tasks that share
//! no memory and communicate *only* through channels.
//!
//! * A **server** lane owns the computation. It receives request messages on
//!   the request channel, computes a reply, and sends it on the response
//!   channel — then loops. When the request channel closes it exits.
//! * A **client** lane reads command lines from the console, sends each as a
//!   request, awaits the reply, and prints it. When input ends it closes the
//!   channels, which unblocks and stops the server.
//!
//! Each request is the UTF-8 bytes of `<op> <a> <b>` (`add`, `sub`, `mul`,
//! `div`); each reply is the result text or an `error: …` message. The whole
//! exchange is driven by the real kernel scheduler with Catch-and-Release on the
//! channels — a parked `recv` consumes nothing until the other side sends.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use cibos_sdk::{ChannelDirection, ChannelTerms, System, WeightClass};
use platform_cli::{CliApp, CliContext};

/// Compute a reply for one request payload. Pure, so it is unit-testable on its
/// own, independent of the channel plumbing.
#[must_use]
pub fn compute(request: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(request);
    let mut parts = text.split_whitespace();
    let op = parts.next().unwrap_or("");
    let a = parts.next().and_then(|s| s.parse::<i64>().ok());
    let b = parts.next().and_then(|s| s.parse::<i64>().ok());

    let result: Result<i64, &str> = match (op, a, b) {
        ("add", Some(a), Some(b)) => a.checked_add(b).ok_or("overflow"),
        ("sub", Some(a), Some(b)) => a.checked_sub(b).ok_or("overflow"),
        ("mul", Some(a), Some(b)) => a.checked_mul(b).ok_or("overflow"),
        ("div", Some(_), Some(0)) => Err("division by zero"),
        ("div", Some(a), Some(b)) => a.checked_div(b).ok_or("overflow"),
        _ => Err("usage: <add|sub|mul|div> <a> <b>"),
    };

    match result {
        Ok(v) => v.to_string().into_bytes(),
        Err(e) => format!("error: {e}").into_bytes(),
    }
}

fn channel_terms(name: &str) -> ChannelTerms {
    // Small buffers; enough that a send need not block when the peer is busy.
    ChannelTerms::new(name, ChannelDirection::Bidirectional, 4096, 8).unwrap()
}

/// The calculator service application.
#[derive(Default)]
pub struct CalcService;

impl CalcService {
    /// Create the calculator service app.
    #[must_use]
    pub fn new() -> Self {
        CalcService
    }
}

/// Spawn the server and client lanes wired by request/response channels.
fn launch(system: &System, console: std::sync::Arc<dyn platform_cli::Console>) {
    let requests = system.open_channel(&channel_terms("calc-requests"));
    let responses = system.open_channel(&channel_terms("calc-responses"));

    // Server lane: receive request -> compute -> send reply, until closed.
    let s_req = requests.clone();
    let s_resp = responses.clone();
    system.spawn_with_lane(WeightClass::System, move |lane| async move {
        while let Ok(req) = s_req.recv(lane).await {
            let reply = compute(&req);
            if s_resp.send(lane, reply).await.is_err() {
                break; // response channel closed
            }
        }
    });

    // Client lane: console line -> request -> await reply -> print.
    let c_req = requests;
    let c_resp = responses;
    system.spawn_with_lane(WeightClass::User, move |lane| async move {
        while let Some(line) = console.read_line() {
            if line.trim().is_empty() {
                continue;
            }
            if c_req.send(lane, line.into_bytes()).await.is_err() {
                break;
            }
            match c_resp.recv(lane).await {
                Ok(reply) => console.write_line(&String::from_utf8_lossy(&reply)),
                Err(_) => break,
            }
        }
        // End of input: close the channels so the server lane exits too.
        c_req.close();
        c_resp.close();
    });
}

impl CliApp for CalcService {
    fn name(&self) -> &str {
        "calc-service"
    }

    fn run(&self, ctx: CliContext) {
        launch(&ctx.system, ctx.console.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_cli::{CaptureConsole, CliRunner};
    use std::sync::Arc;

    #[test]
    fn compute_arithmetic() {
        assert_eq!(compute(b"add 2 3"), b"5");
        assert_eq!(compute(b"sub 10 4"), b"6");
        assert_eq!(compute(b"mul 6 7"), b"42");
        assert_eq!(compute(b"div 20 5"), b"4");
        assert_eq!(compute(b"div 1 0"), b"error: division by zero");
        assert_eq!(compute(b"pow 2 3"), b"error: usage: <add|sub|mul|div> <a> <b>");
    }

    #[test]
    fn client_server_round_trip_over_channels() {
        // The client sends each line as a request; the server replies over the
        // channel. Both lanes are scheduled by the real kernel; the captured
        // output is the server's replies, proving the round trip.
        let console = Arc::new(CaptureConsole::new(
            ["add 2 3", "mul 4 5", "div 9 3", "div 5 0"]
                .iter()
                .map(|s| s.to_string()),
        ));
        let mut runner = CliRunner::new(console.clone());
        runner.run(&CalcService::new());

        assert_eq!(
            console.output(),
            vec![
                "5".to_string(),
                "20".to_string(),
                "3".to_string(),
                "error: division by zero".to_string(),
            ]
        );
    }

    #[test]
    fn many_requests_all_answered_in_order() {
        // Stress the request/reply loop: 50 requests must all be answered, in
        // order, with the server consuming nothing while parked between them.
        let inputs: Vec<String> = (0..50).map(|i| format!("add {i} 1")).collect();
        let console = Arc::new(CaptureConsole::new(inputs));
        let mut runner = CliRunner::new(console.clone());
        runner.run(&CalcService::new());

        let out = console.output();
        assert_eq!(out.len(), 50);
        for (i, line) in out.iter().enumerate() {
            assert_eq!(line, &(i as i64 + 1).to_string());
        }
    }
}
