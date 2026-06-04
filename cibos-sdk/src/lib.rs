//! # `cibos-sdk` — CIBOS Application SDK
//!
//! The `std`-side API surface applications link against. An application receives
//! a [`System`] handle and uses it to open [`Channel`]s, spawn concurrent
//! tasks, query its resource limits, and sleep. Concurrency is the CIBOS
//! HIP-native runtime — tasks become lanes, channel and timer waits are
//! Catch-and-Release — not a general-purpose async runtime, and deliberately
//! not tokio.
//!
//! [`AppHost`] runs an [`Application`] against an in-process [`cibos_kernel`].
//! This is the development and test transport: it is how CIBOS applications are
//! written and exercised before hardware exists. The same [`System`] API will
//! be backed by syscalls to a booted CIBOS kernel in production, with no change
//! to application code.
//!
//! ```no_run
//! use cibos_sdk::{Application, AppHost, System};
//! use shared::{CibosProfile, ResourceLimits, WeightClass};
//!
//! struct Hello;
//! impl Application for Hello {
//!     fn name(&self) -> &str { "hello" }
//!     fn start(&self, system: System) {
//!         system.spawn_user(async { /* ... */ });
//!     }
//! }
//!
//! let mut host = AppHost::new(2, [0u8; 32], CibosProfile::Balanced, 64,
//!     ResourceLimits::default_application());
//! host.launch(&Hello);
//! ```

// No unsafe code: an absolute `forbid` for normal builds. The opt-in
// `host-memory-tracking` feature needs an audited `GlobalAlloc` impl (an unsafe
// trait), so it relaxes to `deny` and the one allocator module explicitly,
// narrowly allows unsafe.
#![cfg_attr(not(feature = "host-memory-tracking"), forbid(unsafe_code))]
#![cfg_attr(feature = "host-memory-tracking", deny(unsafe_code))]
#![warn(missing_docs)]

pub mod app;
mod broker;
mod context;
pub mod channel;
pub mod container;
pub mod error;
pub mod fs;
pub mod lane;
pub mod multihost;
pub mod net;
pub mod system;
pub mod time;

/// Host memory accounting; installs a counting global allocator.
#[cfg(feature = "host-memory-tracking")]
#[allow(unsafe_code)]
mod tracking;

pub use app::{AppHost, Application};
pub use multihost::MultiContainerHost;
pub use error::{SdkError, SdkResult};
pub use fs::Filesystem;
pub use lane::{Lane, LaneError};
pub use net::{Gate, Lattice, Link, Listener, NetError};
pub use system::System;
pub use time::{with_timeout, TimeoutError, Timer};

/// The application entry-point attribute. Apply to an `async fn main`.
pub use cibos_macros::main;
/// Wait on several futures at once, completing on the first ready arm.
pub use cibos_macros::select;

// Re-exports applications commonly need, so they depend on just the SDK.
pub use cibos_async_runtime::yield_now;
pub use cibos_kernel::{RecvStep, SendStep};
/// The raw kernel channel returned by [`System::open_channel`]. Most code wants
/// the typed [`Channel`]; this is the lower-level transport for system services.
pub use cibos_kernel::Channel as KernelChannel;
pub use channel::{
    await_channel_request, Channel, ChannelError, ChannelRequest, IncomingRequest, TryReceiveError,
    TrySendError,
};
pub use container::{ChannelCount, ContainerId, MemoryStats};
pub use shared::protocols::ipc::{ChannelDirection, ChannelTerms};
pub use shared::{CibosProfile, LaneId, ResourceLimits, WeightClass};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn terms() -> ChannelTerms {
        ChannelTerms::new("test", ChannelDirection::Bidirectional, 64, 4).unwrap()
    }

    /// An application: producer task sends 0..5, consumer sums them.
    struct ProducerConsumer {
        received: Arc<Mutex<Vec<u8>>>,
    }

    impl Application for ProducerConsumer {
        fn name(&self) -> &str {
            "producer-consumer"
        }
        fn start(&self, system: System) {
            let channel = system.open_channel(&terms());

            let producer_ch = channel.clone();
            system.spawn_with_lane(WeightClass::User, move |lane| async move {
                for i in 0u8..5 {
                    producer_ch.send(lane, std::vec![i]).await.unwrap();
                }
            });

            let consumer_ch = channel;
            let sink = self.received.clone();
            system.spawn_with_lane(WeightClass::User, move |lane| async move {
                for _ in 0..5 {
                    let msg = consumer_ch.recv(lane).await.unwrap();
                    sink.lock().unwrap().push(msg[0]);
                }
            });
        }
    }

    #[test]
    fn runs_producer_consumer_application() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let app = ProducerConsumer {
            received: received.clone(),
        };
        let mut host = AppHost::new(
            1,
            [5u8; 32],
            CibosProfile::Balanced,
            64,
            ResourceLimits::default_application(),
        );
        host.launch(&app);
        assert_eq!(&*received.lock().unwrap(), &[0u8, 1, 2, 3, 4]);
    }

    /// An application whose single task sleeps, then records that it woke.
    struct Sleeper {
        woke: Arc<Mutex<bool>>,
    }

    impl Application for Sleeper {
        fn name(&self) -> &str {
            "sleeper"
        }
        fn start(&self, system: System) {
            let woke = self.woke.clone();
            let sys = system.clone();
            system.spawn_with_lane(WeightClass::User, move |lane| async move {
                sys.sleep(lane, Duration::from_millis(50)).await;
                *woke.lock().unwrap() = true;
            });
        }
    }

    #[test]
    fn host_auto_drives_explicit_sleep() {
        let woke = Arc::new(Mutex::new(false));
        let app = Sleeper { woke: woke.clone() };
        let mut host = AppHost::new(
            1,
            [0u8; 32],
            CibosProfile::Balanced,
            16,
            ResourceLimits::default_application(),
        );
        // The host loop advances the clock to the timer's deadline on its own,
        // so the sleeping task runs to completion within launch — no manual
        // advance needed.
        host.launch(&app);
        assert!(*woke.lock().unwrap(), "host auto-drove the timer to completion");
    }

    #[test]
    fn application_can_spawn_followups() {
        // A task that spawns another task; both must run to completion.
        struct Chained {
            count: Arc<Mutex<u32>>,
        }
        impl Application for Chained {
            fn name(&self) -> &str {
                "chained"
            }
            fn start(&self, system: System) {
                let count = self.count.clone();
                let sys = system.clone();
                system.spawn_user(async move {
                    *count.lock().unwrap() += 1;
                    let count2 = count.clone();
                    sys.spawn_user(async move {
                        *count2.lock().unwrap() += 1;
                    });
                });
            }
        }

        let count = Arc::new(Mutex::new(0u32));
        let app = Chained {
            count: count.clone(),
        };
        let mut host = AppHost::new(
            2,
            [1u8; 32],
            CibosProfile::Performance,
            16,
            ResourceLimits::default_application(),
        );
        host.launch(&app);
        assert_eq!(*count.lock().unwrap(), 2, "both the task and its child ran");
    }

    #[test]
    fn ambient_timer_sleep_uses_execution_context() {
        // The documented free-function timer API takes no system/lane argument;
        // it reads them from the ambient execution context the runner installs.
        // The closure ignores the passed lane id to prove `Timer::sleep` and
        // `now()` resolve it from the context rather than an explicit handle.
        use crate::time::{now, Timer};

        struct AmbientSleeper {
            done: Arc<Mutex<bool>>,
            elapsed_ok: Arc<Mutex<bool>>,
        }
        impl Application for AmbientSleeper {
            fn name(&self) -> &str {
                "ambient-sleeper"
            }
            fn start(&self, system: System) {
                let done = self.done.clone();
                let elapsed_ok = self.elapsed_ok.clone();
                system.spawn_with_lane(WeightClass::User, move |_lane| async move {
                    let t0 = now();
                    Timer::sleep(Duration::from_millis(100)).await;
                    let t1 = now();
                    *elapsed_ok.lock().unwrap() =
                        t1.saturating_duration_since(t0) >= Duration::from_millis(100);
                    *done.lock().unwrap() = true;
                });
            }
        }

        let done = Arc::new(Mutex::new(false));
        let elapsed_ok = Arc::new(Mutex::new(false));
        let app = AmbientSleeper {
            done: done.clone(),
            elapsed_ok: elapsed_ok.clone(),
        };
        let mut host = AppHost::new(
            1,
            [7u8; 32],
            CibosProfile::Balanced,
            16,
            ResourceLimits::default_application(),
        );
        // The host loop auto-advances the clock to the deadline, so the ambient
        // `Timer::sleep` completes within launch and `now()` reflects it.
        host.launch(&app);
        assert!(*done.lock().unwrap(), "ambient timer resumed via auto-driven clock");
        assert!(
            *elapsed_ok.lock().unwrap(),
            "ambient now() reflected the elapsed sleep"
        );
    }

    #[test]
    fn container_reports_resource_limits_from_context() {
        // `container::*` reads the current container's limits from the ambient
        // context, with no system handle threaded through.
        use crate::container;

        struct Inspect {
            memory_bytes: Arc<Mutex<u64>>,
            memory_limit: Arc<Mutex<usize>>,
            max_lanes: Arc<Mutex<u32>>,
        }
        impl Application for Inspect {
            fn name(&self) -> &str {
                "inspect"
            }
            fn start(&self, system: System) {
                let memory_bytes = self.memory_bytes.clone();
                let memory_limit = self.memory_limit.clone();
                let max_lanes = self.max_lanes.clone();
                system.spawn_with_lane(WeightClass::User, move |_lane| async move {
                    let limits = container::get_resource_limits();
                    *memory_bytes.lock().unwrap() = limits.memory_bytes;
                    *max_lanes.lock().unwrap() = limits.max_lanes;
                    *memory_limit.lock().unwrap() = container::memory_limit();
                });
            }
        }

        let expected = ResourceLimits::default_application();
        let memory_bytes = Arc::new(Mutex::new(0u64));
        let memory_limit = Arc::new(Mutex::new(0usize));
        let max_lanes = Arc::new(Mutex::new(0u32));
        let app = Inspect {
            memory_bytes: memory_bytes.clone(),
            memory_limit: memory_limit.clone(),
            max_lanes: max_lanes.clone(),
        };
        let mut host = AppHost::new(1, [3u8; 32], CibosProfile::Balanced, 16, expected);
        host.launch(&app);
        assert_eq!(*memory_bytes.lock().unwrap(), expected.memory_bytes);
        assert_eq!(*max_lanes.lock().unwrap(), expected.max_lanes);
        assert_eq!(
            *memory_limit.lock().unwrap(),
            expected.memory_bytes as usize
        );
    }

    #[test]
    fn select_completes_on_first_ready_arm() {
        // Both arms are immediately ready; arms are polled in written order, so
        // the first arm wins and its output is bound to its pattern.
        struct Selector {
            out: Arc<Mutex<u32>>,
        }
        impl Application for Selector {
            fn name(&self) -> &str {
                "selector"
            }
            fn start(&self, system: System) {
                let out = self.out.clone();
                system.spawn_with_lane(WeightClass::User, move |_lane| async move {
                    let winner = crate::select! {
                        a = async { 10u32 } => a + 1,
                        b = async { 20u32 } => b + 2,
                    };
                    *out.lock().unwrap() = winner;
                });
            }
        }

        let out = Arc::new(Mutex::new(0u32));
        let app = Selector { out: out.clone() };
        let mut host = AppHost::new(
            1,
            [0u8; 32],
            CibosProfile::Balanced,
            16,
            ResourceLimits::default_application(),
        );
        host.launch(&app);
        assert_eq!(*out.lock().unwrap(), 11, "first ready arm (10 + 1) should win");
    }

    #[test]
    fn lane_create_submit_join_reuse() {
        // A main lane creates a worker lane, submits work, joins, then reuses
        // the same lane for a second future. Reaching count == 2 proves `join`
        // freed the lane (otherwise the second `submit` returns AlreadyOccupied).
        struct LaneApp {
            count: Arc<Mutex<u32>>,
        }
        impl Application for LaneApp {
            fn name(&self) -> &str {
                "lane-reuse"
            }
            fn start(&self, system: System) {
                let count = self.count.clone();
                system.spawn_with_lane(WeightClass::User, move |_main| async move {
                    let mut lane = crate::Lane::create().expect("reserve a lane");
                    assert!(lane.id() != crate::context::current_lane(), "worker id differs from main");

                    let c1 = count.clone();
                    lane.submit(async move {
                        *c1.lock().unwrap() += 1;
                    })
                    .expect("first submit");
                    lane.join().await;

                    let c2 = count.clone();
                    lane.submit(async move {
                        *c2.lock().unwrap() += 1;
                    })
                    .expect("second submit after join");
                    lane.join().await;

                    lane.destroy().expect("destroy an idle lane");
                });
            }
        }

        let count = Arc::new(Mutex::new(0u32));
        let app = LaneApp {
            count: count.clone(),
        };
        let mut host = AppHost::new(
            2,
            [0u8; 32],
            CibosProfile::Balanced,
            16,
            ResourceLimits::default_application(),
        );
        host.launch(&app);
        assert_eq!(
            *count.lock().unwrap(),
            2,
            "both submitted futures ran, proving join freed the lane for reuse"
        );
    }

    #[test]
    fn channel_new_local_backpressure_and_close() {
        // A producer lane sends five values through a capacity-4 channel (so the
        // fifth send back-pressures until the consumer drains), then closes it.
        // The consumer drains until `receive` returns `None`. A total of 15
        // proves every value was delivered and that close terminated the loop.
        struct ChannelApp {
            total: Arc<Mutex<u64>>,
        }
        impl Application for ChannelApp {
            fn name(&self) -> &str {
                "channel-app"
            }
            fn start(&self, system: System) {
                let total = self.total.clone();
                system.spawn_with_lane(WeightClass::User, move |_main| async move {
                    let (tx, rx) = crate::Channel::<u64>::new_local(4).expect("new_local");

                    let mut producer = crate::Lane::create().expect("producer lane");
                    producer
                        .submit(async move {
                            for i in 1..=5u64 {
                                tx.send(i).await.expect("send");
                            }
                            tx.close();
                        })
                        .expect("submit producer");

                    let mut sum = 0u64;
                    while let Some(v) = rx.receive().await {
                        sum += v;
                    }
                    *total.lock().unwrap() = sum;

                    producer.join().await;
                    producer.destroy().expect("destroy idle producer lane");
                });
            }
        }

        let total = Arc::new(Mutex::new(0u64));
        let app = ChannelApp {
            total: total.clone(),
        };
        let mut host = AppHost::new(
            2,
            [0u8; 32],
            CibosProfile::Balanced,
            16,
            ResourceLimits::default_application(),
        );
        host.launch(&app);
        assert_eq!(
            *total.lock().unwrap(),
            15,
            "all five values delivered across back-pressure, close ended the loop"
        );
    }

    #[cfg(feature = "dynamic-weights")]
    #[test]
    fn lane_weighted_create_and_update() {
        // On the Compute profile the kernel honors per-lane weights. Verifies
        // range validation, weighted creation, a runtime update, and that the
        // weighted lane still runs to completion.
        struct Weighted {
            ran: Arc<Mutex<bool>>,
            rejected_zero: Arc<Mutex<bool>>,
            rejected_high: Arc<Mutex<bool>>,
        }
        impl Application for Weighted {
            fn name(&self) -> &str {
                "weighted"
            }
            fn start(&self, system: System) {
                let ran = self.ran.clone();
                let rz = self.rejected_zero.clone();
                let rh = self.rejected_high.clone();
                system.spawn_with_lane(WeightClass::User, move |_lane| async move {
                    *rz.lock().unwrap() = matches!(
                        crate::Lane::create_with_weight(0),
                        Err(LaneError::WeightOutOfRange)
                    );
                    *rh.lock().unwrap() = matches!(
                        crate::Lane::create_with_weight(101),
                        Err(LaneError::WeightOutOfRange)
                    );

                    let mut lane = crate::Lane::create_with_weight(5).expect("valid weight");
                    lane.update_weight(3).expect("dynamic weight update");

                    let r = ran.clone();
                    lane.submit(async move {
                        *r.lock().unwrap() = true;
                    })
                    .expect("submit");
                    lane.join().await;
                    lane.destroy().expect("destroy weighted lane");
                });
            }
        }

        let ran = Arc::new(Mutex::new(false));
        let rejected_zero = Arc::new(Mutex::new(false));
        let rejected_high = Arc::new(Mutex::new(false));
        let app = Weighted {
            ran: ran.clone(),
            rejected_zero: rejected_zero.clone(),
            rejected_high: rejected_high.clone(),
        };
        // Compute profile so the kernel actually applies the weight overrides.
        let mut host = AppHost::new(
            2,
            [0u8; 32],
            CibosProfile::Compute,
            16,
            ResourceLimits::default_application(),
        );
        host.launch(&app);
        assert!(*rejected_zero.lock().unwrap(), "weight 0 rejected");
        assert!(*rejected_high.lock().unwrap(), "weight 101 rejected");
        assert!(*ran.lock().unwrap(), "weighted lane ran to completion");
    }

    #[test]
    fn container_id_is_a_real_nonsystem_boundary() {
        // The host registers each application as a user container; `id()` returns
        // that real boundary — stable within a run and distinct from the reserved
        // system boundary.
        struct IdApp {
            id_ok: Arc<Mutex<bool>>,
        }
        impl Application for IdApp {
            fn name(&self) -> &str {
                "id-app"
            }
            fn start(&self, system: System) {
                let id_ok = self.id_ok.clone();
                system.spawn_with_lane(WeightClass::User, move |_lane| async move {
                    let a = crate::container::id();
                    let b = crate::container::id();
                    *id_ok.lock().unwrap() = a == b && a != shared::BoundaryId::SYSTEM;
                });
            }
        }

        let id_ok = Arc::new(Mutex::new(false));
        let app = IdApp {
            id_ok: id_ok.clone(),
        };
        let mut host = AppHost::new(
            1,
            [0u8; 32],
            CibosProfile::Balanced,
            16,
            ResourceLimits::default_application(),
        );
        host.launch(&app);
        assert!(
            *id_ok.lock().unwrap(),
            "container::id() is stable and distinct from the system boundary"
        );
    }

    #[test]
    fn channel_count_tracks_open_local_channels() {
        // Opening two local channels reports local == 2; closing one reports 1;
        // inbound/outbound are always zero on the host transport.
        struct CountApp {
            snapshot: Arc<Mutex<(u32, u32, u32, u32)>>,
        }
        impl Application for CountApp {
            fn name(&self) -> &str {
                "count-app"
            }
            fn start(&self, system: System) {
                let snapshot = self.snapshot.clone();
                system.spawn_with_lane(WeightClass::User, move |_lane| async move {
                    let (a_tx, _a_rx) = crate::Channel::<u8>::new_local(2).expect("channel a");
                    let (_b_tx, _b_rx) = crate::Channel::<u8>::new_local(2).expect("channel b");

                    let opened = crate::container::channel_count();
                    a_tx.close();
                    let after_close = crate::container::channel_count();

                    *snapshot.lock().unwrap() = (
                        opened.local,
                        after_close.local,
                        after_close.inbound,
                        after_close.outbound,
                    );
                });
            }
        }

        let snapshot = Arc::new(Mutex::new((0u32, 0u32, 0u32, 0u32)));
        let app = CountApp {
            snapshot: snapshot.clone(),
        };
        let mut host = AppHost::new(
            1,
            [0u8; 32],
            CibosProfile::Balanced,
            16,
            ResourceLimits::default_application(),
        );
        host.launch(&app);
        assert_eq!(
            *snapshot.lock().unwrap(),
            (2, 1, 0, 0),
            "two open, one after close; no cross-container channels"
        );
    }

    #[test]
    fn new_local_enforces_max_channels() {
        // With a container limit of two channels, the first two open and the
        // third is rejected while they are still held open.
        struct LimitApp {
            outcome: Arc<Mutex<(bool, bool, bool)>>,
        }
        impl Application for LimitApp {
            fn name(&self) -> &str {
                "channel-limit"
            }
            fn start(&self, system: System) {
                let outcome = self.outcome.clone();
                system.spawn_with_lane(WeightClass::User, move |_lane| async move {
                    let first = crate::Channel::<u8>::new_local(1);
                    let second = crate::Channel::<u8>::new_local(1);
                    // Both held open, so this exceeds the ceiling of two.
                    let third = crate::Channel::<u8>::new_local(1);
                    *outcome.lock().unwrap() = (
                        first.is_ok(),
                        second.is_ok(),
                        matches!(third, Err(crate::ChannelError::TermsViolation)),
                    );
                    // first/second/third stay alive until here.
                });
            }
        }

        let outcome = Arc::new(Mutex::new((false, false, false)));
        let app = LimitApp {
            outcome: outcome.clone(),
        };
        let limits = ResourceLimits {
            max_channels: 2,
            ..ResourceLimits::default_application()
        };
        let mut host = AppHost::new(1, [0u8; 32], CibosProfile::Balanced, 16, limits);
        host.launch(&app);
        assert_eq!(
            *outcome.lock().unwrap(),
            (true, true, true),
            "first two open; third rejected at the channel ceiling"
        );
    }

    #[test]
    fn multi_container_isolated_systems() {
        use shared::BoundaryId;

        let observations: Arc<Mutex<Vec<(BoundaryId, u32)>>> = Arc::new(Mutex::new(Vec::new()));

        let mut host =
            crate::MultiContainerHost::new(2, [0u8; 32], CibosProfile::Balanced, 32);
        let sys_a = host.add_container(ResourceLimits::default_application());
        let sys_b = host.add_container(ResourceLimits::default_application());

        let obs_a = observations.clone();
        sys_a.spawn_with_lane(WeightClass::User, move |_lane| async move {
            let id = crate::container::id();
            // Open one local channel and observe the count while it is alive.
            let _ch = crate::Channel::<u8>::new_local(1).expect("channel a");
            let local = crate::container::channel_count().local;
            obs_a.lock().unwrap().push((id, local));
        });

        let obs_b = observations.clone();
        sys_b.spawn_with_lane(WeightClass::User, move |_lane| async move {
            let id = crate::container::id();
            // This container opened no channels.
            let local = crate::container::channel_count().local;
            obs_b.lock().unwrap().push((id, local));
        });

        host.run();

        let obs = observations.lock().unwrap();
        assert_eq!(obs.len(), 2, "both containers ran");
        assert_ne!(obs[0].0, obs[1].0, "containers have distinct boundaries");
        let counts: std::collections::BTreeSet<u32> = obs.iter().map(|(_, n)| *n).collect();
        assert_eq!(
            counts,
            [0u32, 1u32].into_iter().collect(),
            "channel accounting is isolated per container (one opened a channel, the other did not)"
        );
    }

    #[test]
    fn cross_container_channel_request_accept_flow() {
        use shared::BoundaryId;

        let received: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        // (requester outbound count, accepter inbound count) observed while open.
        let counts: Arc<Mutex<(u32, u32)>> = Arc::new(Mutex::new((0, 0)));

        let mut host = crate::MultiContainerHost::new(2, [0u8; 32], CibosProfile::Balanced, 32);
        let sys_a = host.add_container(ResourceLimits::default_application());
        let sys_b = host.add_container(ResourceLimits::default_application());
        let target: BoundaryId = sys_b.boundary();

        // Accepter: await the request, accept, drain values.
        let recv_b = received.clone();
        let counts_b = counts.clone();
        sys_b.spawn_with_lane(WeightClass::User, move |_lane| async move {
            let incoming = crate::await_channel_request::<u64>()
                .await
                .expect("await request");
            let channel = incoming.accept();
            counts_b.lock().unwrap().1 = crate::container::channel_count().inbound;
            while let Some(v) = channel.receive().await {
                recv_b.lock().unwrap().push(v);
            }
        });

        // Requester: request a channel to the accepter, send three values, close.
        let counts_a = counts.clone();
        sys_a.spawn_with_lane(WeightClass::User, move |_lane| async move {
            let channel = crate::Channel::<u64>::request(crate::ChannelRequest {
                target,
                purpose: "demo",
                buffer_capacity: 4,
            })
            .await
            .expect("request accepted");
            counts_a.lock().unwrap().0 = crate::container::channel_count().outbound;
            for i in 1..=3u64 {
                channel.send(i).await.expect("send");
            }
            channel.close();
        });

        host.run();
        assert_eq!(*received.lock().unwrap(), vec![1, 2, 3], "values crossed containers");
        assert_eq!(
            *counts.lock().unwrap(),
            (1, 1),
            "one outbound on the requester, one inbound on the accepter"
        );
    }

    #[cfg(feature = "host-memory-tracking")]
    #[test]
    fn memory_usage_tracks_allocation() {
        // A known 4 MB allocation must be visible in allocated_bytes, retained in
        // peak_bytes, and released on drop; limit_bytes matches the launch limit.
        struct MemApp {
            ok: Arc<Mutex<bool>>,
            limit: Arc<Mutex<u64>>,
        }
        impl Application for MemApp {
            fn name(&self) -> &str {
                "mem-app"
            }
            fn start(&self, system: System) {
                let ok = self.ok.clone();
                let limit = self.limit.clone();
                system.spawn_with_lane(WeightClass::User, move |_lane| async move {
                    const CHUNK: usize = 4_000_000;
                    let before = crate::container::memory_usage();
                    let buf: Vec<u8> = vec![7u8; CHUNK];
                    let during = crate::container::memory_usage();
                    // Touch the buffer so it cannot be optimized away.
                    let touch = u64::from(buf[0]) + u64::from(buf[CHUNK - 1]);
                    drop(buf);
                    let after = crate::container::memory_usage();

                    let grew = during.allocated_bytes >= before.allocated_bytes + CHUNK as u64;
                    let peak_ok = during.peak_bytes >= during.allocated_bytes;
                    let shrank = after.allocated_bytes < during.allocated_bytes;
                    let peak_retained = after.peak_bytes >= during.allocated_bytes;

                    *ok.lock().unwrap() =
                        grew && peak_ok && shrank && peak_retained && touch == 14;
                    *limit.lock().unwrap() = during.limit_bytes;
                });
            }
        }

        let ok = Arc::new(Mutex::new(false));
        let limit = Arc::new(Mutex::new(0u64));
        let app = MemApp {
            ok: ok.clone(),
            limit: limit.clone(),
        };
        let mut host = AppHost::new(
            1,
            [0u8; 32],
            CibosProfile::Balanced,
            16,
            ResourceLimits::default_application(),
        );
        host.launch(&app);
        assert!(
            *ok.lock().unwrap(),
            "memory_usage reflects allocation, peak, and release"
        );
        assert_eq!(
            *limit.lock().unwrap(),
            ResourceLimits::default_application().memory_bytes,
            "limit_bytes matches the launch limit"
        );
    }
}
