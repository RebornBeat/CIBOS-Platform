//! Pipeline processing example (Application Developer Guide, ch. 5).
//!
//! Three lanes form a pipeline wired by local channels: a **source** emits
//! values, a **transform** squares each, and a **sink** sums them. The stages
//! overlap in time, with channel back-pressure pacing the source to the slowest
//! stage and `close` propagating end-of-stream down the pipeline (each stage
//! closes its output once its input drains). The sink reports the total over a
//! one-slot result channel. No timers, so it runs to completion under the host
//! entry.
//!
//! ```text
//! cargo run -p cibos-sdk --example pipeline_processing
//! ```

use cibos_sdk::{Channel, Lane};

const ITEMS: u64 = 8;

#[cibos_sdk::main]
async fn main() {
    let (raw_tx, raw_rx) = Channel::<u64>::new_local(4).expect("raw channel");
    let (proc_tx, proc_rx) = Channel::<u64>::new_local(4).expect("processed channel");
    let (result_tx, result_rx) = Channel::<u64>::new_local(1).expect("result channel");

    let mut source = Lane::create().expect("source lane");
    let mut transform = Lane::create().expect("transform lane");
    let mut sink = Lane::create().expect("sink lane");

    // Stage 1: produce, then close so the transform stage sees end-of-stream.
    source
        .submit(async move {
            for i in 1..=ITEMS {
                raw_tx.send(i).await.expect("send raw");
            }
            raw_tx.close();
        })
        .expect("submit source");

    // Stage 2: transform each item, then close the processed channel.
    transform
        .submit(async move {
            while let Some(x) = raw_rx.receive().await {
                proc_tx.send(x * x).await.expect("send processed");
            }
            proc_tx.close();
        })
        .expect("submit transform");

    // Stage 3: reduce, then report the total back to the main lane.
    sink.submit(async move {
        let mut total = 0u64;
        while let Some(y) = proc_rx.receive().await {
            total += y;
        }
        result_tx.send(total).await.expect("send result");
    })
    .expect("submit sink");

    let total = result_rx.receive().await.expect("receive result");
    println!("pipeline sum of squares (1..={ITEMS}) = {total} (expected 204)");

    for mut lane in [source, transform, sink] {
        lane.join().await;
        lane.destroy().expect("destroy pipeline lane");
    }
}
