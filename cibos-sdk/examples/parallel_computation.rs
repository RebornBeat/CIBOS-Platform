//! Parallel computation example (Application Developer Guide, ch. 5).
//!
//! Demonstrates the documented quantum-like data-parallel pattern composed from
//! the real SDK surface: a worker [`Lane`] per partition, each computing a
//! partial result and sending it back over a local [`Channel`]; the initial lane
//! collects every partial — all pathways preserved, no collapse. It uses no
//! timers, so it runs to completion under the host entry today.
//!
//! ```text
//! cargo run -p cibos-sdk --example parallel_computation
//! ```

use cibos_sdk::{Channel, Lane};

const LANE_COUNT: usize = 4;
const DATA: [u64; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

#[cibos_sdk::main]
async fn main() {
    let (sender, receiver) =
        Channel::<(usize, u64)>::new_local(LANE_COUNT).expect("create result channel");

    let chunk = DATA.len() / LANE_COUNT;
    let mut lanes = Vec::with_capacity(LANE_COUNT);

    for i in 0..LANE_COUNT {
        let partition: Vec<u64> = DATA[i * chunk..(i + 1) * chunk].to_vec();
        let results = sender.clone();
        let mut lane = Lane::create().expect("create compute lane");
        lane.submit(async move {
            // Scientific stand-in: sum of squares of this partition.
            let partial: u64 = partition.iter().map(|&x| x * x).sum();
            results.send((i, partial)).await.expect("send partial");
        })
        .expect("submit compute work");
        lanes.push(lane);
    }
    drop(sender);

    // Collect every partial — all preserved, then resolve by summing.
    let mut total = 0u64;
    for _ in 0..LANE_COUNT {
        let (lane_index, partial) = receiver.receive().await.expect("receive partial");
        println!("lane {lane_index} partial = {partial}");
        total += partial;
    }

    for mut lane in lanes {
        lane.join().await;
        lane.destroy().expect("destroy idle compute lane");
    }

    println!("parallel sum of squares = {total} (expected 650)");
}
