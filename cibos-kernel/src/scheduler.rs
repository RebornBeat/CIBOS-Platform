//! # The Scheduler
//!
//! The kernel's implementation of [`shared::KernelInterface`] and the home of
//! the HIP ready/stalled bookkeeping.
//!
//! * **Ready list** — lanes that can make progress and are awaiting an
//!   execution context.
//! * **Stalled list** — lanes parked on a [`WaitResource`] under
//!   Catch-and-Release, consuming nothing until released.
//!
//! The runtime's wakers call [`signal_ready`](Scheduler::signal_ready) (release)
//! and the runtime's `ResourceWait` futures call
//! [`register_wait`](Scheduler::register_wait) (catch), both through the
//! `KernelInterface`. The kernel's scheduling loop calls
//! [`take_dispatch_batch`](Scheduler::take_dispatch_batch) to choose which ready
//! lanes run next, using the weighted-entropy [`selector`](crate::selector).
//!
//! All mutable state sits behind a single [`SpinLock`]; the lock is released
//! before any lane is polled, so the `register_wait`/`signal_ready` calls a poll
//! makes never re-enter the lock.

use crate::entropy::Csprng;
use crate::selector::{self, WeightedLane};
use crate::sync::SpinLock;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::time::Duration;
use shared::protocols::ipc::WaitResource;
use shared::types::time::Monotonic;
use shared::{CibosProfile, KernelInterface, LaneId, SchedulingConfig, WeightClass};

struct SchedulerState {
    ready: VecDeque<LaneId>,
    stalled: BTreeMap<LaneId, WaitResource>,
    classes: BTreeMap<LaneId, WeightClass>,
    overrides: BTreeMap<LaneId, u32>,
    csprng: Csprng,
    now: Monotonic,
}

/// The CIBOS scheduler.
pub struct Scheduler {
    state: SpinLock<SchedulerState>,
    execution_contexts: usize,
    profile: CibosProfile,
    config: SchedulingConfig,
}

impl Scheduler {
    /// Create a scheduler for `execution_contexts` contexts, seeded with
    /// `entropy_seed`, running under `profile`.
    #[must_use]
    pub fn new(execution_contexts: usize, entropy_seed: [u8; 32], profile: CibosProfile) -> Self {
        Self {
            state: SpinLock::new(SchedulerState {
                ready: VecDeque::new(),
                stalled: BTreeMap::new(),
                classes: BTreeMap::new(),
                overrides: BTreeMap::new(),
                csprng: Csprng::from_seed(entropy_seed),
                now: Monotonic::ZERO,
            }),
            execution_contexts: execution_contexts.max(1),
            profile,
            config: SchedulingConfig::defaults_for(profile),
        }
    }

    /// The kernel profile this scheduler runs under.
    #[must_use]
    pub fn profile(&self) -> CibosProfile {
        self.profile
    }

    /// Whether the active profile permits per-lane / dynamic weight control.
    /// Per the profile rules this is the Compute profile.
    #[must_use]
    fn supports_weight_overrides(&self) -> bool {
        matches!(self.profile, CibosProfile::Compute)
    }

    /// Record a lane's weight class (called when the lane is created).
    pub fn register_lane(&self, lane: LaneId, class: WeightClass) {
        self.state.lock().classes.insert(lane, class);
    }

    /// Remove all bookkeeping for a completed (or aborted) lane.
    pub fn notify_complete(&self, lane: LaneId) {
        let mut s = self.state.lock();
        s.ready.retain(|l| *l != lane);
        s.stalled.remove(&lane);
        s.classes.remove(&lane);
        s.overrides.remove(&lane);
    }

    /// Number of lanes currently ready.
    #[must_use]
    pub fn ready_count(&self) -> usize {
        self.state.lock().ready.len()
    }

    /// Number of lanes currently stalled.
    #[must_use]
    pub fn stalled_count(&self) -> usize {
        self.state.lock().stalled.len()
    }

    /// Whether the scheduler has no ready and no stalled lanes.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        let s = self.state.lock();
        s.ready.is_empty() && s.stalled.is_empty()
    }

    /// Whether at least one lane is ready to run.
    #[must_use]
    pub fn has_ready(&self) -> bool {
        !self.state.lock().ready.is_empty()
    }

    /// Resolve a lane's selection weight from its class (and any override under
    /// a profile that allows overrides).
    fn weight_for(state: &SchedulerState, scheduler: &Scheduler, lane: LaneId) -> u32 {
        if scheduler.supports_weight_overrides() {
            if let Some(w) = state.overrides.get(&lane) {
                return (*w).max(1);
            }
        }
        let class = state.classes.get(&lane).copied().unwrap_or(WeightClass::User);
        match class {
            WeightClass::System => scheduler.config.system_weight,
            WeightClass::User => scheduler.config.user_weight,
            WeightClass::Background => scheduler.config.background_weight,
        }
    }

    /// Choose the lanes to dispatch this scheduling pass and remove them from
    /// the ready list. The caller polls each; lanes that stall or yield re-enter
    /// the lists through `register_wait` / `signal_ready`, and completed lanes
    /// are dropped via [`notify_complete`](Scheduler::notify_complete).
    #[must_use]
    pub fn take_dispatch_batch(&self) -> Vec<LaneId> {
        let mut s = self.state.lock();

        let weighted: Vec<WeightedLane> = s
            .ready
            .iter()
            .map(|&lane| WeightedLane {
                lane,
                weight: Self::weight_for(&s, self, lane),
            })
            .collect();

        let chosen = selector::select(&weighted, self.execution_contexts, &mut s.csprng);
        s.ready.retain(|l| !chosen.contains(l));
        chosen
    }

    /// Advance the monotonic clock by `delta`, releasing any timer waits whose
    /// deadline has now passed (moving them from stalled to ready).
    pub fn advance_clock(&self, delta: Duration) {
        let mut s = self.state.lock();
        s.now = s.now.saturating_add(delta);
        let now = s.now;

        // Collect matured timer lanes, then release them.
        let matured: Vec<LaneId> = s
            .stalled
            .iter()
            .filter_map(|(lane, resource)| match resource {
                WaitResource::Timer(deadline) if now.reached(*deadline) => Some(*lane),
                _ => None,
            })
            .collect();
        for lane in matured {
            s.stalled.remove(&lane);
            if !s.ready.contains(&lane) {
                s.ready.push_back(lane);
            }
        }
    }

    /// Host-transport idle pump. When no lane is ready, advance the monotonic
    /// clock to the earliest pending timer deadline and release every timer wait
    /// that has now matured, mirroring [`advance_clock`](Scheduler::advance_clock).
    /// Returns `true` if a timer wait was found and the clock advanced (so the
    /// caller should keep running), or `false` if no timer waits remain (the
    /// system is genuinely idle — e.g. only non-timer stalls, or nothing stalled).
    ///
    /// Reading the earliest deadline and advancing happen under one lock, so the
    /// released set is consistent with the new `now`. The clock never moves
    /// backward.
    pub fn advance_to_next_timer(&self) -> bool {
        let mut s = self.state.lock();
        let earliest = s
            .stalled
            .values()
            .filter_map(|resource| match resource {
                WaitResource::Timer(deadline) => Some(*deadline),
                _ => None,
            })
            .min();
        let Some(deadline) = earliest else {
            return false;
        };

        // Jump to the deadline unless the clock is already at or past it.
        if !s.now.reached(deadline) {
            s.now = deadline;
        }
        let now = s.now;

        let matured: Vec<LaneId> = s
            .stalled
            .iter()
            .filter_map(|(lane, resource)| match resource {
                WaitResource::Timer(d) if now.reached(*d) => Some(*lane),
                _ => None,
            })
            .collect();
        for lane in matured {
            s.stalled.remove(&lane);
            if !s.ready.contains(&lane) {
                s.ready.push_back(lane);
            }
        }
        true
    }
}

impl KernelInterface for Scheduler {
    fn register_wait(&self, lane: LaneId, resource: WaitResource) {
        let mut s = self.state.lock();
        s.ready.retain(|l| *l != lane);
        s.stalled.insert(lane, resource);
    }

    fn signal_ready(&self, lane: LaneId) {
        let mut s = self.state.lock();
        s.stalled.remove(&lane);
        if !s.ready.contains(&lane) {
            s.ready.push_back(lane);
        }
    }

    fn now(&self) -> Monotonic {
        self.state.lock().now
    }

    fn set_lane_weight(&self, lane: LaneId, weight: u32) -> bool {
        if !self.supports_weight_overrides() {
            return false;
        }
        self.state.lock().overrides.insert(lane, weight.max(1));
        true
    }

    fn update_lane_weight(&self, lane: LaneId, weight: u32) -> bool {
        self.set_lane_weight(lane, weight)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_signal_move_between_lists() {
        let sched = Scheduler::new(2, [0u8; 32], CibosProfile::Balanced);
        let lane = LaneId::new(1);
        sched.signal_ready(lane);
        assert_eq!(sched.ready_count(), 1);
        assert_eq!(sched.stalled_count(), 0);

        sched.register_wait(lane, WaitResource::Memory(1024));
        assert_eq!(sched.ready_count(), 0);
        assert_eq!(sched.stalled_count(), 1);

        sched.signal_ready(lane);
        assert_eq!(sched.ready_count(), 1);
        assert_eq!(sched.stalled_count(), 0);
    }

    #[test]
    fn signal_ready_is_idempotent() {
        let sched = Scheduler::new(2, [0u8; 32], CibosProfile::Balanced);
        let lane = LaneId::new(1);
        sched.signal_ready(lane);
        sched.signal_ready(lane);
        assert_eq!(sched.ready_count(), 1, "a lane appears in ready at most once");
    }

    #[test]
    fn dispatch_batch_respects_contexts() {
        let sched = Scheduler::new(2, [3u8; 32], CibosProfile::Balanced);
        for id in 1..=5 {
            sched.signal_ready(LaneId::new(id));
        }
        let batch = sched.take_dispatch_batch();
        assert_eq!(batch.len(), 2, "only as many as execution contexts");
        // Dispatched lanes were removed from ready.
        assert_eq!(sched.ready_count(), 3);
    }

    #[test]
    fn timer_matures_on_clock_advance() {
        let sched = Scheduler::new(1, [0u8; 32], CibosProfile::Balanced);
        let lane = LaneId::new(1);
        sched.register_wait(lane, WaitResource::Timer(Monotonic::from_millis(10)));
        assert_eq!(sched.stalled_count(), 1);

        // Not yet due.
        sched.advance_clock(Duration::from_millis(5));
        assert_eq!(sched.ready_count(), 0);
        assert_eq!(sched.stalled_count(), 1);

        // Now due: released to ready.
        sched.advance_clock(Duration::from_millis(6));
        assert_eq!(sched.ready_count(), 1);
        assert_eq!(sched.stalled_count(), 0);
    }

    #[test]
    fn weight_override_only_under_compute() {
        let balanced = Scheduler::new(1, [0u8; 32], CibosProfile::Balanced);
        assert!(!balanced.set_lane_weight(LaneId::new(1), 9));

        let compute = Scheduler::new(1, [0u8; 32], CibosProfile::Compute);
        assert!(compute.set_lane_weight(LaneId::new(1), 9));
    }

    #[test]
    fn maximum_isolation_forces_equal_weights() {
        // Under Maximum Isolation the config weights are all 1, so a System lane
        // and a Background lane resolve to the same selection weight.
        let sched = Scheduler::new(1, [0u8; 32], CibosProfile::MaximumIsolation);
        sched.register_lane(LaneId::new(1), WeightClass::System);
        sched.register_lane(LaneId::new(2), WeightClass::Background);
        let s = sched.state.lock();
        assert_eq!(Scheduler::weight_for(&s, &sched, LaneId::new(1)), 1);
        assert_eq!(Scheduler::weight_for(&s, &sched, LaneId::new(2)), 1);
    }
}
