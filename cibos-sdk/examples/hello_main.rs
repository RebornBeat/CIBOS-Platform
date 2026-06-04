//! Minimal `#[cibos::main]` example.
//!
//! Boots the host entry point, runs the body on the initial lane, and reads the
//! container's resource limits through the ambient execution context — proving
//! the entry macro establishes that context with no system handle threaded in.
//!
//! It deliberately avoids `Timer::sleep` and the `Lane` facade: the host entry
//! does not yet auto-advance timers, and the stateful `Lane` API awaits the
//! scheduler-reachable lane-reservation increment. Run it with:
//!
//! ```text
//! cargo run -p cibos-sdk --example hello_main
//! ```

use cibos_sdk::container;

#[cibos_sdk::main]
async fn main() {
    let limits = container::get_resource_limits();
    println!(
        "hello from a lane — memory_limit={} bytes, max_lanes={}, max_channels={}",
        container::memory_limit(),
        limits.max_lanes,
        limits.max_channels,
    );
}
