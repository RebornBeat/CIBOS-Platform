//! # The System Handle
//!
//! [`System`] is the capability an application uses to interact with CIBOS:
//! spawn concurrent tasks, open channels, and query its own resource limits and
//! the clock. It is a cheap, cloneable handle — an application clones it into
//! each task it spawns.
//!
//! Spawns are **queued** rather than applied immediately. A task spawning a
//! child runs *inside* a kernel poll, where the executor is already borrowed;
//! queuing the child and letting the host runner install it on the next
//! scheduling pass keeps the borrow discipline clean while preserving the
//! natural "spawn and the child runs soon" semantics.
//!
//! Concurrency here is the CIBOS HIP-native runtime, not a general-purpose
//! async runtime: tasks become lanes, channel waits are Catch-and-Release.

use crate::error::SdkResult;
use crate::broker::ChannelBroker;
use crate::fs::Filesystem;
use crate::net::Lattice;
use cibos_async_runtime::ResourceWait;
use cibos_kernel::Channel;
use shared::protocols::ipc::{ChannelTerms, WaitResource};
use shared::types::time::Monotonic;
use shared::{BoundaryId, ChannelId, KernelInterface, LaneId, ResourceLimits, WeightClass};
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

/// A boxed, type-erased task future.
type TaskFuture = Pin<Box<dyn Future<Output = ()>>>;

/// A pending spawn awaiting installation by the host runner. The lane id is
/// minted by the SDK (the host transport's lane-id authority) before queueing,
/// and the future is already wrapped in its ambient-context wrapper.
pub(crate) struct PendingSpawn {
    pub(crate) class: WeightClass,
    pub(crate) lane: LaneId,
    pub(crate) future: TaskFuture,
}

struct SystemInner {
    kernel_if: std::sync::Arc<dyn KernelInterface>,
    limits: ResourceLimits,
    boundary: BoundaryId,
    ids: Rc<IdSource>,
    active_lanes: Cell<u32>,
    open_local_channels: Cell<u32>,
    outbound_channels: Cell<u32>,
    inbound_channels: Cell<u32>,
    broker: Rc<ChannelBroker>,
    pending: RefCell<Vec<PendingSpawn>>,
    filesystem: Filesystem,
    lattice: Lattice,
}

/// A source of globally-unique lane and channel ids shared by every container
/// running on one kernel. The executor keys lanes and the scheduler keys waits
/// on these ids, so they must not collide across containers; a host hands every
/// container the same `IdSource` (single-threaded host, so a `Cell` suffices).
pub(crate) struct IdSource {
    next_lane: Cell<u64>,
    next_channel: Cell<u64>,
}

impl IdSource {
    /// A fresh id source, ids starting at 1.
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(IdSource {
            next_lane: Cell::new(1),
            next_channel: Cell::new(1),
        })
    }

    fn mint_lane(&self) -> u64 {
        let id = self.next_lane.get();
        self.next_lane.set(id + 1);
        id
    }

    fn mint_channel(&self) -> u64 {
        let id = self.next_channel.get();
        self.next_channel.set(id + 1);
        id
    }
}

/// An application's handle to CIBOS. Clone freely; all clones share the same
/// underlying connection.
#[derive(Clone)]
pub struct System {
    inner: Rc<SystemInner>,
}

impl System {
    /// Construct a system handle over a kernel interface, with the application's
    /// resource limits, the isolation [`BoundaryId`] of its container, and the
    /// shared [`IdSource`] for the kernel it runs on (so lane and channel ids are
    /// unique across all containers sharing that kernel). Used by the host runner;
    /// applications receive a `System` rather than building one.
    #[must_use]
    pub(crate) fn new(
        kernel_if: std::sync::Arc<dyn KernelInterface>,
        limits: ResourceLimits,
        boundary: BoundaryId,
        ids: Rc<IdSource>,
        broker: Rc<ChannelBroker>,
    ) -> Self {
        System {
            inner: Rc::new(SystemInner {
                kernel_if,
                limits,
                boundary,
                ids,
                active_lanes: Cell::new(0),
                open_local_channels: Cell::new(0),
                outbound_channels: Cell::new(0),
                inbound_channels: Cell::new(0),
                broker,
                pending: RefCell::new(Vec::new()),
                filesystem: Filesystem::new(),
                lattice: Lattice::new(),
            }),
        }
    }

    /// The shared cross-container broker for the host this system runs on.
    pub(crate) fn broker(&self) -> Rc<ChannelBroker> {
        self.inner.broker.clone()
    }

    /// The isolation boundary (container id) this application runs in.
    #[must_use]
    pub fn boundary(&self) -> BoundaryId {
        self.inner.boundary
    }

    /// The shared network fabric (Lattice). All tasks of this system share it.
    #[must_use]
    pub fn lattice(&self) -> Lattice {
        self.inner.lattice.clone()
    }

    /// The shared filesystem service. All tasks of this system see one
    /// filesystem; cloning the handle shares the same store.
    #[must_use]
    pub fn filesystem(&self) -> Filesystem {
        self.inner.filesystem.clone()
    }

    /// This application's resource limits.
    #[must_use]
    pub fn resource_limits(&self) -> ResourceLimits {
        self.inner.limits
    }

    /// The current monotonic time.
    #[must_use]
    pub fn now(&self) -> Monotonic {
        self.inner.kernel_if.now()
    }

    /// A future that sleeps `duration` from now, on behalf of `lane`. Resolves
    /// when the kernel clock passes the deadline (Catch-and-Release on a timer).
    #[must_use]
    pub fn sleep(&self, lane: LaneId, duration: Duration) -> ResourceWait {
        let deadline = self.now().saturating_add(duration);
        ResourceWait::new(
            self.inner.kernel_if.clone(),
            lane,
            WaitResource::Timer(deadline),
        )
    }

    /// Open a channel with the given terms. Both the returned handle and any
    /// clones share one buffer.
    #[must_use]
    pub fn open_channel(&self, terms: &ChannelTerms) -> Channel {
        let id = self.inner.ids.mint_channel();
        Channel::new(ChannelId::new(id), terms, self.inner.kernel_if.clone())
    }

    /// Mint the next lane id and count it as an in-flight slot. The SDK is the
    /// host transport's lane-id authority, so ids are unique and ascending.
    fn mint_lane_counted(&self) -> LaneId {
        let id = self.inner.ids.mint_lane();
        self.inner
            .active_lanes
            .set(self.inner.active_lanes.get() + 1);
        LaneId::new(id)
    }

    /// Spawn a concurrent task in the given weight `class`. Fire-and-forget: its
    /// lane slot is released when the task completes.
    pub fn spawn<F>(&self, class: WeightClass, future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        let lane = self.mint_lane_counted();
        let wrapped: TaskFuture = Box::pin(crate::context::WithLaneContext::new(
            lane,
            self.clone(),
            true,
            Box::pin(future),
        ));
        self.inner.pending.borrow_mut().push(PendingSpawn {
            class,
            lane,
            future: wrapped,
        });
    }

    /// Spawn a concurrent task that needs its own lane id (to send or receive on
    /// channels). The builder receives the assigned [`LaneId`]. Fire-and-forget:
    /// its lane slot is released when the task completes.
    pub fn spawn_with_lane<B, F>(&self, class: WeightClass, build: B)
    where
        B: FnOnce(LaneId) -> F + 'static,
        F: Future<Output = ()> + 'static,
    {
        let lane = self.mint_lane_counted();
        let inner: TaskFuture = Box::pin(build(lane));
        let wrapped: TaskFuture = Box::pin(crate::context::WithLaneContext::new(
            lane,
            self.clone(),
            true,
            inner,
        ));
        self.inner.pending.borrow_mut().push(PendingSpawn {
            class,
            lane,
            future: wrapped,
        });
    }

    /// Try to reserve a lane slot, returning its id. Returns `None` if the
    /// container is already at its `max_lanes` limit. The slot is held until
    /// explicitly released (see [`release_reserved_lane`]); used by the `Lane`
    /// handle, whose slot persists across `submit`/`join` cycles.
    ///
    /// [`release_reserved_lane`]: System::release_reserved_lane
    pub(crate) fn try_reserve_lane(&self) -> Option<LaneId> {
        if self.inner.active_lanes.get() >= self.inner.limits.max_lanes {
            return None;
        }
        Some(self.mint_lane_counted())
    }

    /// Spawn `future` on an already-reserved lane `id`. The slot was counted at
    /// reservation, so this neither mints nor counts; the slot is released on
    /// `destroy`, not on completion (`release_on_complete = false`).
    pub(crate) fn spawn_on_reserved<F>(&self, class: WeightClass, lane: LaneId, future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        let wrapped: TaskFuture = Box::pin(crate::context::WithLaneContext::new(
            lane,
            self.clone(),
            false,
            Box::pin(future),
        ));
        self.inner.pending.borrow_mut().push(PendingSpawn {
            class,
            lane,
            future: wrapped,
        });
    }

    /// Release one in-flight lane slot (a fire-and-forget task completed). Used
    /// by the ambient-context wrapper and by the host runner to undo a count
    /// when a spawn is rejected by the executor.
    pub(crate) fn note_lane_complete(&self) {
        let active = self.inner.active_lanes.get();
        if active > 0 {
            self.inner.active_lanes.set(active - 1);
        }
    }

    /// Release a reserved lane slot held by a `Lane` handle (on `destroy`).
    pub(crate) fn release_reserved_lane(&self) {
        self.note_lane_complete();
    }

    /// Record that an application-visible local channel was opened
    /// (`Channel::new_local`). Internal channels opened via `open_channel`
    /// (lane-join, doorbells) are not counted.
    pub(crate) fn note_local_channel_open(&self) {
        self.inner
            .open_local_channels
            .set(self.inner.open_local_channels.get() + 1);
    }

    /// Record that an application-visible local channel was closed.
    pub(crate) fn note_local_channel_closed(&self) {
        let open = self.inner.open_local_channels.get();
        if open > 0 {
            self.inner.open_local_channels.set(open - 1);
        }
    }

    /// The number of application-visible local channels currently open.
    pub(crate) fn local_channel_count(&self) -> u32 {
        self.inner.open_local_channels.get()
    }

    /// Record that an outbound cross-container channel was established.
    pub(crate) fn note_outbound_channel_open(&self) {
        self.inner
            .outbound_channels
            .set(self.inner.outbound_channels.get() + 1);
    }

    /// Record that an outbound cross-container channel was torn down.
    pub(crate) fn note_outbound_channel_closed(&self) {
        let open = self.inner.outbound_channels.get();
        if open > 0 {
            self.inner.outbound_channels.set(open - 1);
        }
    }

    /// The number of outbound cross-container channels currently open.
    pub(crate) fn outbound_channel_count(&self) -> u32 {
        self.inner.outbound_channels.get()
    }

    /// Record that an inbound cross-container channel was accepted.
    pub(crate) fn note_inbound_channel_open(&self) {
        self.inner
            .inbound_channels
            .set(self.inner.inbound_channels.get() + 1);
    }

    /// Record that an inbound cross-container channel was torn down.
    pub(crate) fn note_inbound_channel_closed(&self) {
        let open = self.inner.inbound_channels.get();
        if open > 0 {
            self.inner.inbound_channels.set(open - 1);
        }
    }

    /// The number of inbound cross-container channels currently open.
    pub(crate) fn inbound_channel_count(&self) -> u32 {
        self.inner.inbound_channels.get()
    }

    /// Set a lane's scheduling weight (per-lane-weights). Best-effort: the
    /// kernel applies it only under a profile that supports per-lane weights
    /// (Compute); elsewhere the lane keeps its class weight. Returns whether the
    /// kernel applied it.
    #[cfg(feature = "per-lane-weights")]
    pub(crate) fn set_lane_weight(&self, lane: LaneId, weight: u32) -> bool {
        self.inner.kernel_if.set_lane_weight(lane, weight)
    }

    /// Update a lane's scheduling weight at runtime (dynamic-weights).
    /// Best-effort, as for [`set_lane_weight`](System::set_lane_weight).
    #[cfg(feature = "dynamic-weights")]
    pub(crate) fn update_lane_weight(&self, lane: LaneId, weight: u32) -> bool {
        self.inner.kernel_if.update_lane_weight(lane, weight)
    }

    /// Convenience: spawn a user-class task.
    pub fn spawn_user<F>(&self, future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        self.spawn(WeightClass::User, future);
    }

    /// Whether there are no queued spawns. Used by the host runner's loop.
    #[must_use]
    pub(crate) fn pending_is_empty(&self) -> bool {
        self.inner.pending.borrow().is_empty()
    }

    /// Take all queued spawns, used by the host runner to install them.
    pub(crate) fn take_pending(&self) -> Vec<PendingSpawn> {
        std::mem::take(&mut *self.inner.pending.borrow_mut())
    }

    /// Validate that a proposed allocation fits this application's limits.
    ///
    /// # Errors
    ///
    /// [`SdkError::LimitExceeded`](crate::SdkError::LimitExceeded) is not
    /// returned here directly; this returns the shared check result mapped into
    /// an SDK result.
    pub fn check_allocation(&self, already: u64, requested: u64) -> SdkResult<()> {
        self.inner
            .limits
            .check_allocation(already, requested)
            .map_err(|e| crate::SdkError::from(shared::SharedError::from(e)))
    }
}

// ---- App-seam trait impls (cibos-console) ------------------------------------
//
// These let the *same* line-oriented application logic (e.g. `shell::dispatch`)
// run against the host SDK here and against the on-kernel `cibos-app` runtime,
// by abstracting exactly the surface that logic uses. The bodies delegate to the
// existing inherent methods, so host behavior is unchanged.

impl cibos_console::ShellFs for Filesystem {
    fn write(&self, path: &str, data: &[u8]) -> bool {
        Filesystem::write(self, path, data)
    }
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        Filesystem::read(self, path)
    }
    fn list(&self, path: &str) -> Vec<String> {
        Filesystem::list(self, path)
    }
    fn delete(&self, path: &str) -> bool {
        Filesystem::delete(self, path)
    }
    fn exists(&self, path: &str) -> bool {
        Filesystem::exists(self, path)
    }
}

impl cibos_console::ShellSystem for System {
    type Fs = Filesystem;

    fn filesystem(&self) -> Filesystem {
        System::filesystem(self)
    }
    fn now_nanos(&self) -> u64 {
        System::now(self).as_nanos()
    }
    fn resource_limits(&self) -> shared::ResourceLimits {
        System::resource_limits(self)
    }
}
