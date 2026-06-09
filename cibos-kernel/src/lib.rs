//! # `cibos-kernel` — CIBOS Kernel Core
//!
//! The kernel implements [`shared::KernelInterface`] and runs the HIP scheduler
//! that drives the async runtime. This crate currently provides the scheduling
//! core — the part where the HIP guarantees actually live:
//!
//! * [`entropy`] — a deterministic, seed-driven CSPRNG (SHA-256 hash-counter).
//! * [`selector`] — the weighted-entropy dispatch decision.
//! * [`scheduler`] — the ready/stalled bookkeeping and `KernelInterface` impl.
//! * [`kernel`] — the [`Kernel`] that runs the scheduling loop over the
//!   real [`cibos_async_runtime::LaneExecutor`].
//! * [`sync`] — a minimal [`SpinLock`](sync::SpinLock) for `Sync` interior
//!   mutability.
//!
//! Container/boundary management, channels, the memory manager, and two-phase
//! initialization from the firmware handoff build on this core and are added
//! next.
//!
//! ## `no_std`
//!
//! `no_std` with `alloc`. The kernel binary provides the global allocator;
//! `std` is enabled only for the host test suite, which runs the real scheduler
//! against the real executor.

#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]

extern crate alloc;

pub mod address_space;
pub mod channel;
pub mod container;
pub mod entropy;
pub mod error;
pub mod frame;
pub mod kernel;
pub mod memory;
pub mod paging;
pub mod scheduler;
pub mod syscall;
pub mod selector;
pub mod sync;

pub use address_space::AddressSpaceManager;
pub use channel::{Channel, ChannelRegistry, RecvStep, SendStep};
pub use container::ContainerRegistry;
pub use error::{KernelError, KernelResult};
pub use frame::{FrameAllocator, PhysFrame, FRAME_SIZE};
pub use kernel::Kernel;
pub use memory::MemoryManager;
pub use paging::{AddressSpace, PageTableEncoder, Permissions};
pub use scheduler::Scheduler;
pub use selector::{select, WeightedLane};
pub use syscall::{dispatch as dispatch_syscall, SyscallEnv, SyscallOutcome, SyscallRequest};

/// The operational profile this kernel binary was *compiled* for, as selected by
/// exactly one `profile-*` feature bundle, or `None` when built without one
/// (host tests and tooling).
///
/// This is the compile-time counterpart to the runtime [`shared::CibosProfile`]
/// the kernel receives in the firmware handoff. The boot path asserts the two
/// agree — a binary compiled as one profile refuses a handoff claiming another —
/// which is the kernel-side half of ADR-007's guarantee that prohibited
/// mechanisms do not exist in a given binary. `build.rs` permits at most one
/// bundle, so at most one arm below is compiled in.
#[must_use]
pub const fn compiled_profile() -> Option<shared::CibosProfile> {
    #[cfg(feature = "profile-maximum-isolation")]
    {
        return Some(shared::CibosProfile::MaximumIsolation);
    }
    #[cfg(feature = "profile-balanced")]
    {
        return Some(shared::CibosProfile::Balanced);
    }
    #[cfg(feature = "profile-performance")]
    {
        return Some(shared::CibosProfile::Performance);
    }
    #[cfg(feature = "profile-compute")]
    {
        return Some(shared::CibosProfile::Compute);
    }
    #[cfg(not(any(
        feature = "profile-maximum-isolation",
        feature = "profile-balanced",
        feature = "profile-performance",
        feature = "profile-compute"
    )))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use cibos_async_runtime::{yield_now, ResourceWait};
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::time::Duration;
    use shared::protocols::ipc::WaitResource;
    use shared::types::time::Monotonic;
    use shared::{CibosProfile, WeightClass};

    #[test]
    fn runs_independent_lanes_to_completion() {
        let mut kernel = Kernel::new(2, [11u8; 32], CibosProfile::Balanced, 64);
        let counter = Arc::new(AtomicUsize::new(0));

        for _ in 0..3 {
            let c = counter.clone();
            kernel
                .spawn(WeightClass::User, async move {
                    c.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap();
        }

        kernel.run_until_idle();
        assert_eq!(counter.load(Ordering::SeqCst), 3);
        assert!(kernel.is_idle());
        assert_eq!(kernel.lane_count(), 0);
    }

    #[test]
    fn yielding_lane_completes() {
        let mut kernel = Kernel::new(1, [0u8; 32], CibosProfile::Performance, 16);
        let steps = Arc::new(AtomicUsize::new(0));
        let s = steps.clone();
        kernel
            .spawn(WeightClass::User, async move {
                s.fetch_add(1, Ordering::SeqCst);
                yield_now().await;
                s.fetch_add(1, Ordering::SeqCst);
                yield_now().await;
                s.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();

        kernel.run_until_idle();
        assert_eq!(steps.load(Ordering::SeqCst), 3);
        assert!(kernel.is_idle());
    }

    #[test]
    fn timer_stalled_lane_releases_on_clock_advance() {
        let mut kernel = Kernel::new(1, [5u8; 32], CibosProfile::Balanced, 16);
        let fired = Arc::new(AtomicUsize::new(0));
        let f = fired.clone();
        let interface = kernel.interface();

        kernel
            .spawn_with_lane(WeightClass::User, move |lane| async move {
                ResourceWait::new(
                    interface,
                    lane,
                    WaitResource::Timer(Monotonic::from_millis(10)),
                )
                .await;
                f.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();

        // First run: the lane registers its timer wait and parks.
        kernel.run_until_idle();
        assert_eq!(fired.load(Ordering::SeqCst), 0, "timer not yet due");
        assert_eq!(kernel.scheduler().stalled_count(), 1);
        assert!(!kernel.is_idle(), "a stalled lane remains");

        // Advance past the deadline; the timer matures and the lane runs.
        kernel.advance_clock(Duration::from_millis(15));
        kernel.run_until_idle();
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert!(kernel.is_idle());
    }

    #[test]
    fn many_lanes_under_competition_all_eventually_complete() {
        // 1 execution context, 20 lanes: heavy competition. Weighted-entropy
        // selection picks one at a time, but every lane must eventually run.
        let mut kernel = Kernel::new(1, [99u8; 32], CibosProfile::Balanced, 64);
        let done = Arc::new(AtomicUsize::new(0));
        for _ in 0..20 {
            let d = done.clone();
            kernel
                .spawn(WeightClass::User, async move {
                    yield_now().await;
                    d.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap();
        }
        kernel.run_until_idle();
        assert_eq!(done.load(Ordering::SeqCst), 20, "no lane starved");
        assert!(kernel.is_idle());
    }

    #[test]
    fn lane_limit_propagates_as_kernel_error() {
        let mut kernel = Kernel::new(1, [0u8; 32], CibosProfile::Compute, 1);
        let interface = kernel.interface();
        // Occupy the single slot with a lane that parks forever.
        kernel
            .spawn_with_lane(WeightClass::User, move |lane| async move {
                ResourceWait::new(interface, lane, WaitResource::Memory(1)).await;
            })
            .unwrap();
        let second = kernel.spawn(WeightClass::User, async {});
        assert!(matches!(second, Err(KernelError::Runtime(_))));
    }

    #[test]
    fn boots_from_firmware_handoff_and_runs() {
        use shared::protocols::handoff::{HandoffData, ENTROPY_SEED_LEN};
        use shared::{
            CibiosProfile, CoreTopology, HandoffMode, HardwarePlatform, MemoryRegion,
            MemoryRegionKind, ProcessorArchitecture,
        };

        // Construct a handoff as CIBIOS would: Standard firmware -> Balanced
        // kernel, 4 cores, 1 GiB usable.
        let topology = CoreTopology::new(4, 4, false).unwrap();
        let regions = [
            MemoryRegion {
                base: 0,
                length: 0x10_0000,
                kind: MemoryRegionKind::FirmwareReserved,
            },
            MemoryRegion {
                base: 0x10_0000,
                length: 0x4000_0000,
                kind: MemoryRegionKind::Usable,
            },
        ];
        let handoff = HandoffData::new(
            ProcessorArchitecture::X86_64,
            HardwarePlatform::Desktop,
            CibiosProfile::Standard,
            CibosProfile::Balanced,
            HandoffMode::Cryptographic,
            topology,
            0x4000_0000,
            &regions,
            [3u8; ENTROPY_SEED_LEN],
        )
        .unwrap();

        let mut kernel = Kernel::from_handoff(&handoff, 64).expect("kernel boots");

        // Phase-2 invariants: system boundary exists, memory accounted.
        assert!(kernel.containers().contains(kernel.system_boundary()));
        assert_eq!(kernel.memory().total_usable(), 0x4000_0000);
        assert_eq!(kernel.memory().region_count(), 1);

        // The booted kernel actually runs lanes.
        let ran = Arc::new(AtomicUsize::new(0));
        let r = ran.clone();
        kernel
            .spawn(WeightClass::System, async move {
                r.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        kernel.run_until_idle();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert!(kernel.is_idle());
    }

    #[test]
    fn rejects_handoff_with_forbidden_pairing() {
        use shared::protocols::handoff::{HandoffData, ENTROPY_SEED_LEN};
        use shared::{
            CibiosProfile, CoreTopology, HandoffMode, HardwarePlatform, ProcessorArchitecture,
        };

        // Standard firmware must not launch a Compute kernel; from_handoff must
        // reject this at bringup.
        let topology = CoreTopology::new(1, 1, false).unwrap();
        let handoff = HandoffData::new(
            ProcessorArchitecture::X86_64,
            HardwarePlatform::Server,
            CibiosProfile::Standard,
            CibosProfile::Compute,
            HandoffMode::Cryptographic,
            topology,
            1024,
            &[],
            [0u8; ENTROPY_SEED_LEN],
        )
        .unwrap();

        assert!(Kernel::from_handoff(&handoff, 16).is_err());
    }

    #[test]
    fn producer_consumer_over_channel_with_backpressure() {
        use alloc::vec::Vec;
        use shared::protocols::ipc::{ChannelDirection, ChannelTerms};

        // One execution context and a 2-deep channel, but three messages: the
        // producer must block on back-pressure and be released by the consumer,
        // all coordinated by the real scheduler.
        let mut kernel = Kernel::new(1, [77u8; 32], CibosProfile::Balanced, 16);
        let terms = ChannelTerms::new("demo", ChannelDirection::RequesterToReceiver, 64, 2).unwrap();
        let channel = kernel.create_channel(&terms);

        // Producer lane: send 3 messages.
        let prod_ch = channel.clone();
        kernel
            .spawn_with_lane(WeightClass::User, move |lane| async move {
                for i in 0u8..3 {
                    prod_ch.send(lane, alloc::vec![i]).await.unwrap();
                }
            })
            .unwrap();

        // Consumer lane: receive 3 messages into a shared sink.
        let received = Arc::new(crate::sync::SpinLock::new(Vec::<u8>::new()));
        let sink = received.clone();
        let cons_ch = channel.clone();
        kernel
            .spawn_with_lane(WeightClass::User, move |lane| async move {
                for _ in 0..3 {
                    let msg = cons_ch.recv(lane).await.unwrap();
                    sink.lock().push(msg[0]);
                }
            })
            .unwrap();

        kernel.run_until_idle();

        assert!(kernel.is_idle(), "both lanes finished");
        assert_eq!(&*received.lock(), &[0u8, 1, 2], "all messages delivered in order");
    }
}
