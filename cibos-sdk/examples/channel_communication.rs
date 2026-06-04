//! Channel communication example (Application Developer Guide, ch. 5).
//!
//! Two containers on one host connect over a cross-container channel: a
//! **sender** requests a channel to a **receiver** by its container id, the
//! receiver accepts, and messages flow across the boundary. The request/accept
//! handshake is routed through the host's broker; once connected, the channel is
//! an ordinary typed [`Channel`] on both sides.
//!
//! Cross-container channels need more than one container, so this example uses
//! [`MultiContainerHost`] rather than the single-application `#[cibos::main]`.
//!
//! ```text
//! cargo run -p cibos-sdk --example channel_communication
//! ```

use cibos_sdk::{
    await_channel_request, container, Channel, ChannelRequest, CibosProfile, MultiContainerHost,
    ResourceLimits, WeightClass,
};

fn main() {
    let mut host = MultiContainerHost::new(2, [0u8; 32], CibosProfile::Balanced, 32);
    let sender = host.add_container(ResourceLimits::default_application());
    let receiver = host.add_container(ResourceLimits::default_application());
    let receiver_id = receiver.boundary();

    // Receiver: accept the incoming request and print each message until close.
    receiver.spawn_with_lane(WeightClass::User, move |_lane| async move {
        let incoming = await_channel_request::<u32>()
            .await
            .expect("await channel request");
        println!(
            "receiver: request from {:?} (purpose '{}')",
            incoming.source(),
            incoming.purpose()
        );
        let channel = incoming.accept();
        println!("receiver: inbound channels = {}", container::channel_count().inbound);
        while let Some(message) = channel.receive().await {
            println!("receiver: got {message}");
        }
        println!("receiver: channel closed");
    });

    // Sender: request a channel to the receiver, then send three messages.
    sender.spawn_with_lane(WeightClass::User, move |_lane| async move {
        let channel = Channel::<u32>::request(ChannelRequest {
            target: receiver_id,
            purpose: "greetings",
            buffer_capacity: 4,
        })
        .await
        .expect("request accepted");
        println!("sender: outbound channels = {}", container::channel_count().outbound);
        for message in [10u32, 20, 30] {
            channel.send(message).await.expect("send");
            println!("sender: sent {message}");
        }
        channel.close();
    });

    host.run();
    println!("channel-communication complete");
}
