//! Hello Lane — the minimal CIBOS application (Getting Started Guide, ch. 3).
//!
//! Creates a lane, submits work that computes a greeting and then sleeps on a
//! timer, and joins it. The submitted future parks on `Timer::sleep`; the host
//! loop advances the monotonic clock to the deadline on its own, so the lane
//! resumes and completes — no manual clock control. Run with:
//!
//! ```text
//! cargo run -p cibos-sdk --example hello_lane
//! ```

use cibos_sdk::{Lane, Timer};
use core::time::Duration;

#[cibos_sdk::main]
async fn main() {
    println!("Hello Lane starting...");

    let mut lane = Lane::create().expect("lane creation failed");

    lane.submit(async {
        let greeting = compute_greeting("CIBOS");
        println!("Computed: {greeting}");

        // `.await` here parks the lane on a timer; the host drives the clock
        // forward so it resumes after the deadline.
        Timer::sleep(Duration::from_millis(100)).await;

        println!("Timer fired. Lane complete.");
    })
    .expect("lane submit failed");

    lane.join().await;

    println!("Hello Lane complete.");
}

/// Pure computation — no async, no I/O.
fn compute_greeting(target: &str) -> String {
    format!("Hello from a lane, {target}!")
}
