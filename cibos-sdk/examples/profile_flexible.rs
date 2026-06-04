//! Profile-flexible computation (Application Developer Guide — writing one
//! application that adapts to the compiled profile).
//!
//! The same source builds and runs whether or not per-lane scheduling weights
//! are available. When the `per-lane-weights` feature is compiled in (the
//! Compute profile), each worker lane is created with a scheduling weight;
//! otherwise it falls back to a plain lane. The computation is identical either
//! way — only the scheduling hint differs — which is the point of profile
//! flexibility: application logic is written once and the profile decides how it
//! is scheduled.
//!
//! ```text
//! cargo run -p cibos-sdk --example profile_flexible
//! cargo run -p cibos-sdk --example profile_flexible --features per-lane-weights
//! ```

use cibos_sdk::{Channel, Lane};

const WORKERS: usize = 3;

#[cfg(feature = "per-lane-weights")]
const WEIGHTS_MODE: &str = "enabled (weighted lanes)";
#[cfg(not(feature = "per-lane-weights"))]
const WEIGHTS_MODE: &str = "disabled (plain lanes)";

#[cibos_sdk::main]
async fn main() {
    println!("profile-flexible: per-lane weights {WEIGHTS_MODE}");

    let (sender, receiver) = Channel::<u64>::new_local(WORKERS).expect("result channel");
    let mut lanes = Vec::with_capacity(WORKERS);

    for i in 0..WORKERS {
        // Profile-adaptive lane creation: weighted where supported, plain otherwise.
        #[cfg(feature = "per-lane-weights")]
        let mut lane = Lane::create_with_weight((i as u32 + 1) * 10).expect("weighted lane");
        #[cfg(not(feature = "per-lane-weights"))]
        let mut lane = Lane::create().expect("lane");

        let results = sender.clone();
        lane.submit(async move {
            // Identical work regardless of profile: sum of 1..=(100 * (i + 1)).
            let partial: u64 = (1..=((i as u64 + 1) * 100)).sum();
            results.send(partial).await.expect("send partial");
        })
        .expect("submit work");
        lanes.push(lane);
    }
    drop(sender);

    let mut total = 0u64;
    for _ in 0..WORKERS {
        total += receiver.receive().await.expect("receive partial");
    }

    for mut lane in lanes {
        lane.join().await;
        lane.destroy().expect("destroy lane");
    }

    // 5050 + 20100 + 45150 = 70300, independent of profile.
    println!("profile-flexible total = {total} (expected 70300)");
}
