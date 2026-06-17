//! Selector-owned ring-3 lane table — the table-driven multi-lane model.
//!
//! This is step 3 of the live-context design: the kernel holds N ring-3 lanes,
//! each with its own saved context, and the canonical `cibos_kernel::Scheduler`
//! (Ready/Stalled lists + weighted-entropy selection) decides which lane runs
//! next. A lane that stalls is parked (its full context already saved by the
//! trap stub into `*CURRENT_USER_CTX`); the selector dispatches another ready
//! lane; a parked lane is later resumed exactly where it trapped.
//!
//! Faithful to the canonical single-selector cooperative model:
//!   * ONE selector (`cibos_kernel::Scheduler`) owns Ready/Stalled — we do not
//!     invent a parallel scheduler; we delegate policy to it.
//!   * Weighted entropy is applied only under competition (the Scheduler's
//!     `take_dispatch_batch` does exactly that).
//!   * Cooperative only — a lane runs until it traps/stalls/exits; no preemption.
//!
//! The arch-specific per-lane state (the `SavedUserContext`, the user stack)
//! lives here in `kernel-image`; selection policy is the shared Scheduler's.
//! The resume assembly (`resume_user_context` / `CURRENT_USER_CTX`) is reused
//! UNCHANGED — it already takes the context pointer as an argument.

extern crate alloc;
use alloc::collections::BTreeMap;

use cibos_kernel::sync::SpinLock;
use cibos_kernel::Scheduler;
use shared::protocols::ipc::{KernelInterface, WaitResource};
use shared::types::time::Monotonic;
use shared::{BoundaryId, LaneId, WeightClass};

use crate::arch::gdt;
use crate::arch::ring3_ctx::SavedUserContext;
use crate::boot::KernelReturnContext;

/// The live selector-owned ring-3 lane table, reachable from the syscall path
/// (`KernelSyscallEnv::spawn`) and the trap (boundary lookup). Mirrors the
/// `CHANNEL_TABLE`/`ROOT_FS` static pattern. Installed for the duration of a
/// multilane run and cleared after. Lock discipline is BRIEF: never held across
/// `resume_user_context` (the trap re-enters and locks the same static), so no
/// deadlock — exactly like the channel syscall's brief locks.
pub static RING3_TABLE: SpinLock<Option<Ring3Table>> = SpinLock::new(None);

/// Default user RFLAGS: IF (bit 9) set so the lane runs with interrupts enabled,
/// plus the reserved bit 1. Matches `enter_user.s`.
const USER_RFLAGS: u64 = 0x202;

/// One ring-3 lane's kernel-side state.
struct Ring3Lane {
    /// The lane's saved CPU context (resumable). Stable address: the table owns
    /// it, and `CURRENT_USER_CTX` is pointed here while the lane runs.
    ctx: SavedUserContext,
    /// The security principal this lane runs as — the real boundary the trap
    /// will read (step 4) instead of the hardcoded SYSTEM.
    boundary: BoundaryId,
    /// Set once the lane has run at least once (so its `exit` routes through
    /// `return_to_saved_kernel`).
    started: bool,
    /// `Some(code)` once the lane has exited.
    exited: Option<i64>,
}

/// The selector-owned ring-3 lane table. Holds the per-lane contexts; delegates
/// Ready/Stalled selection to the canonical `Scheduler`. The scheduler is shared
/// (Arc) so canonical `Channel`s use the SAME scheduler as their
/// `KernelInterface` for back-pressure wakeups — a lane that parks on a full/empty
/// channel buffer is woken by the same selector that runs the other endpoint.
pub struct Ring3Table {
    lanes: BTreeMap<LaneId, Ring3Lane>,
    scheduler: alloc::sync::Arc<Scheduler>,
    next_id: u64,
}

impl Ring3Table {
    /// Create a table with its own selector (single selector, as the model
    /// requires). `execution_contexts` is 1 for the cooperative demo (one ring-3
    /// lane runs at a time); the selection math is unchanged for more.
    #[must_use]
    pub fn new(entropy_seed: [u8; 32]) -> Self {
        use shared::CibosProfile;
        Self {
            lanes: BTreeMap::new(),
            scheduler: alloc::sync::Arc::new(Scheduler::new(1, entropy_seed, CibosProfile::Compute)),
            next_id: 1,
        }
    }

    /// The shared scheduler, usable as the `KernelInterface` backing canonical
    /// `Channel`s so channel back-pressure wakeups go through the SAME selector
    /// that dispatches ring-3 lanes.
    #[must_use]
    pub fn scheduler(&self) -> alloc::sync::Arc<Scheduler> {
        self.scheduler.clone()
    }

    /// Register a new ring-3 lane: build its initial saved context (entry point,
    /// stack, user selectors), register it with the selector, and mark it ready.
    /// Returns the lane id. The caller has already mapped `entry`/`stack_top`
    /// into the lane's address space.
    pub fn spawn_lane(
        &mut self,
        entry: u64,
        stack_top: u64,
        arg: u64,
        boundary: BoundaryId,
        class: WeightClass,
    ) -> LaneId {
        let lane = LaneId(self.next_id);
        self.next_id += 1;

        let mut ctx = SavedUserContext::empty();
        ctx.rip = entry;
        ctx.rsp = stack_top;
        ctx.cs = gdt::USER_CODE as u64;
        ctx.ss = gdt::USER_DATA as u64;
        ctx.rflags = USER_RFLAGS;
        // The spawned lane receives `arg` as its first argument in rdi (the SysV
        // first-argument register), matching the ring-3 SDK `spawn(entry, arg)`
        // wrapper. The entry payload reads rdi to obtain its argument word.
        ctx.rdi = arg;

        self.lanes.insert(
            lane,
            Ring3Lane {
                ctx,
                boundary,
                started: false,
                exited: None,
            },
        );
        self.scheduler.register_lane(lane, class);
        // Mark ready (Catch-and-Release: a lane with work enters the Ready list).
        self.scheduler.signal_ready(lane);
        lane
    }

    /// The boundary a lane runs as (read by the trap in step 4).
    #[must_use]
    pub fn boundary_of(&self, lane: LaneId) -> Option<BoundaryId> {
        self.lanes.get(&lane).map(|l| l.boundary)
    }

    /// The exit code of a finished lane, if it has exited.
    #[must_use]
    pub fn exit_code(&self, lane: LaneId) -> Option<i64> {
        self.lanes.get(&lane).and_then(|l| l.exited)
    }

    /// Whether the selector still has ready or stalled lanes.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.scheduler.has_ready() || self.scheduler.stalled_count() > 0
    }

    /// Take the next dispatch batch (weighted entropy only under competition).
    #[must_use]
    pub fn take_batch(&self) -> alloc::vec::Vec<LaneId> {
        self.scheduler.take_dispatch_batch()
    }

    /// Advance the selector clock, releasing matured timer waits.
    pub fn advance(&self, millis: u64) {
        self.scheduler
            .advance_clock(core::time::Duration::from_millis(millis));
    }

    /// A raw pointer to a lane's saved context. STABLE across `BTreeMap` inserts
    /// (values live in boxed nodes), so a later `spawn_lane` does not invalidate
    /// a pointer taken here for a currently-running lane. Returns null if absent
    /// or already exited.
    fn ctx_ptr(&mut self, lane: LaneId) -> *mut SavedUserContext {
        match self.lanes.get_mut(&lane) {
            Some(e) if e.exited.is_none() => {
                e.started = true;
                core::ptr::addr_of_mut!(e.ctx)
            }
            _ => core::ptr::null_mut(),
        }
    }

    /// Record that a lane parked on `resource` (re-stall it).
    fn park(&self, lane: LaneId, resource: WaitResource) {
        self.scheduler.register_wait(lane, resource);
    }

    /// Record that a lane exited with `code` (drop it from the selector).
    fn complete(&mut self, lane: LaneId, code: i64) {
        if let Some(e) = self.lanes.get_mut(&lane) {
            e.exited = Some(code);
        }
        self.scheduler.notify_complete(lane);
    }
}

extern "C" {
    fn resume_user_context(ctx: *const SavedUserContext, kctx: *mut KernelReturnContext) -> i64;
}

/// Run the cooperative selector loop against the installed `RING3_TABLE` until no
/// lane is ready or stalled. LOCK-SAFE: the table lock is taken only briefly
/// (to pick the batch, take a ctx pointer, record park/exit) and is ALWAYS
/// released before `resume_user_context`, so a syscall issued by the running
/// lane (`spawn`, channel ops, boundary lookup) can re-lock the table without
/// deadlock — the same brief-lock discipline as the channel syscall.
///
/// `on_park` decides what a parked (yielded) lane waits for.
///
/// # Safety
/// Must run with GDT/TSS + IDT installed, the context-saving syscall vector
/// active, and the lanes' pages mapped in the active address space. The table
/// must already be installed in `RING3_TABLE`. Single-threaded bring-up.
pub unsafe fn run_installed(mut on_park: impl FnMut(LaneId) -> Option<WaitResource>) {
    let mut kret = KernelReturnContext::zeroed();
    let mut guard = 0u32;

    loop {
        guard += 1;
        if guard > 100_000 {
            break; // defensive: never spin forever in bring-up
        }

        // --- brief lock: is there work, and what's the batch? ---
        let batch = {
            let mut t = RING3_TABLE.lock();
            let Some(table) = t.as_mut() else { break };
            if !table.busy() {
                break;
            }
            table.take_batch()
        };

        if batch.is_empty() {
            // No lane ready this pass: advance the clock to release timer waits.
            let mut t = RING3_TABLE.lock();
            if let Some(table) = t.as_mut() {
                table.advance(1);
            }
            continue;
        }

        for lane in batch {
            // --- brief lock: take the lane's ctx pointer + mark it current ---
            let ctx = {
                let mut t = RING3_TABLE.lock();
                let Some(table) = t.as_mut() else { break };
                let p = table.ctx_ptr(lane);
                if p.is_null() {
                    continue;
                }
                crate::boot::set_current_user_ctx(p);
                crate::boot::set_active_lane(lane);
                p
            }; // <-- lock RELEASED here, before resuming the lane

            // Resume the lane WITHOUT holding the table lock: the lane may issue
            // syscalls (spawn / channels) that re-lock RING3_TABLE.
            let code = resume_user_context(ctx, core::ptr::addr_of_mut!(kret));

            // --- brief lock: record park or exit ---
            crate::boot::clear_active_lane();
            let mut t = RING3_TABLE.lock();
            let Some(table) = t.as_mut() else { break };
            if code == crate::loader::PARKED_SENTINEL {
                match on_park(lane) {
                    Some(resource) => table.park(lane, resource),
                    None => table.scheduler.signal_ready(lane),
                }
            } else {
                table.complete(lane, code);
            }
        }

        // Release matured timer waits after each pass.
        let mut t = RING3_TABLE.lock();
        if let Some(table) = t.as_mut() {
            table.advance(1);
        }
        let _ = Monotonic::ZERO;
    }
}

// ---- Multilane demonstration (feature: ring3-multilane-demo) ---------------
//
// Two ring-3 lanes in one address space. The canonical Scheduler picks which
// runs; lane A logs "step 1" then YIELDS (parks); the selector dispatches lane
// B which logs + exits; then the selector resumes lane A from the trap point and
// it logs "resumed step 2" + exits. The serial order is the proof that the
// selector switched lanes at the park and came back — table-driven multi-context.

/// Lane A: log "step 1", yield, log "resumed step 2", exit(0).
#[rustfmt::skip]
const LANE_A: &[u8] = &[
    72, 199, 192, 1, 0, 0, 0, 72, 141, 61, 130, 0, 0, 0, 72, 199,
    198, 18, 0, 0, 0, 72, 49, 210, 205, 128, 72, 199, 192, 17, 0, 0,
    0, 72, 191, 0, 0, 0, 3, 0, 80, 0, 0, 72, 199, 198, 66, 0,
    0, 0, 72, 49, 210, 205, 128, 72, 199, 192, 1, 0, 0, 0, 72, 141,
    61, 107, 0, 0, 0, 72, 199, 198, 27, 0, 0, 0, 72, 49, 210, 205,
    128, 72, 199, 192, 3, 0, 0, 0, 72, 49, 255, 205, 128, 72, 199, 192,
    1, 0, 0, 0, 72, 141, 61, 101, 0, 0, 0, 72, 199, 198, 26, 0,
    0, 0, 72, 49, 210, 205, 128, 72, 199, 192, 2, 0, 0, 0, 72, 49,
    255, 205, 128, 235, 254, 102, 102, 46, 15, 31, 132, 0, 0, 0, 0, 0,
    32, 32, 91, 108, 97, 110, 101, 32, 65, 93, 32, 115, 116, 101, 112, 32,
    49, 10, 144, 102, 102, 46, 15, 31, 132, 0, 0, 0, 0, 0, 102, 144,
    32, 32, 91, 108, 97, 110, 101, 32, 65, 93, 32, 115, 112, 97, 119, 110,
    101, 100, 32, 97, 32, 99, 104, 105, 108, 100, 10, 144, 15, 31, 64, 0,
    32, 32, 91, 108, 97, 110, 101, 32, 65, 93, 32, 114, 101, 115, 117, 109,
    101, 100, 32, 115, 116, 101, 112, 32, 50, 10,
];

/// Lane B: log "step 1", exit(0).
#[rustfmt::skip]
const LANE_B: &[u8] = &[
    72, 199, 192, 1, 0, 0, 0, 72, 141, 61, 26, 0, 0, 0, 72, 199,
    198, 18, 0, 0, 0, 72, 49, 210, 205, 128, 72, 199, 192, 2, 0, 0,
    0, 72, 49, 255, 205, 128, 235, 254, 32, 32, 91, 108, 97, 110, 101, 32,
    66, 93, 32, 115, 116, 101, 112, 32, 49, 10, 0,
];

/// Child lane spawned at runtime BY lane A via the `spawn` syscall: log + exit.
#[rustfmt::skip]
const LANE_C: &[u8] = &[
    72, 137, 251, 72, 199, 192, 1, 0, 0, 0, 72, 141, 61, 31, 0, 0,
    0, 72, 199, 198, 32, 0, 0, 0, 72, 49, 210, 205, 128, 72, 199, 192,
    2, 0, 0, 0, 72, 137, 223, 205, 128, 235, 254, 15, 31, 68, 0, 0,
    32, 32, 91, 99, 104, 105, 108, 100, 93, 32, 115, 112, 97, 119, 110, 101,
    100, 32, 98, 121, 32, 108, 97, 110, 101, 32, 65, 32, 114, 97, 110, 10,
];

// Per-lane virtual layout (distinct code+stack pages so both coexist in one
// space). Lane 0 uses the loader's default pair; lane 1 sits 16 MiB above.
const LANE0_CODE: u64 = 0x0000_5000_0000_0000;
const LANE0_STACK: u64 = 0x0000_5000_0010_0000;
const LANE1_CODE: u64 = 0x0000_5000_0100_0000;
const LANE1_STACK: u64 = 0x0000_5000_0110_0000;
/// Where the child payload (spawned at runtime by lane A) is mapped. Lane A's
/// `spawn` syscall passes this as the entry point.
const CHILD_CODE: u64 = 0x0000_5000_0300_0000;

/// Run the selector-owned multilane demo. Maps two lanes into a fresh space,
/// spawns them in the Ring3Table, and runs the cooperative selector loop.
///
/// # Safety
/// Must run with GDT/TSS + IDT installed and the context-saving syscall vector
/// active (the caller installs/restores it). Single-threaded bring-up.
pub unsafe fn run_multilane_demo(
    frames: &cibos_kernel::FrameAllocator,
    phys_to_ptr: &impl Fn(u64) -> *mut u8,
    entropy_seed: [u8; 32],
) {
    kprintln!("CIBOS kernel: ring-3 multilane (selector-owned table) demo starting");

    let kernel_root = crate::arch::paging::current_root();
    let space = match crate::loader::new_lane_space(frames, phys_to_ptr) {
        Ok(s) => s,
        Err(e) => {
            kprintln!("  multilane: {} — skipping", e);
            return;
        }
    };

    // Map both lanes into the one space.
    let a_stack = match crate::loader::map_lane(&space, frames, LANE_A, LANE0_CODE, LANE0_STACK, phys_to_ptr) {
        Ok(s) => s,
        Err(e) => { kprintln!("  multilane: lane A map {} — skipping", e); return; }
    };
    let b_stack = match crate::loader::map_lane(&space, frames, LANE_B, LANE1_CODE, LANE1_STACK, phys_to_ptr) {
        Ok(s) => s,
        Err(e) => { kprintln!("  multilane: lane B map {} — skipping", e); return; }
    };
    // Map the child payload (no stack — the spawn syscall maps its stack) so
    // lane A's runtime `spawn(CHILD_CODE)` has a valid entry to start.
    if let Err(e) = crate::loader::map_lane_code(&space, frames, LANE_C, CHILD_CODE, phys_to_ptr) {
        kprintln!("  multilane: child code map {} — skipping", e);
        return;
    }

    // Build the table and spawn the two lanes (distinct boundaries — proving the
    // table carries the real principal per lane). Install it into RING3_TABLE so
    // the syscall path (spawn) and the trap (boundary lookup) can reach it.
    let mut table = Ring3Table::new(entropy_seed);
    let a = table.spawn_lane(LANE0_CODE, a_stack, 0, BoundaryId(0x100), WeightClass::User);
    let b = table.spawn_lane(LANE1_CODE, b_stack, 0, BoundaryId(0x200), WeightClass::User);
    kprintln!(
        "  spawned 2 ring-3 lanes: A=#{} (boundary {:#x}), B=#{} (boundary {:#x})",
        a.0, table.boundary_of(a).unwrap().0, b.0, table.boundary_of(b).unwrap().0
    );
    // Share the selector's scheduler with the channel system so channel
    // back-pressure wakeups go through the SAME selector that runs the lanes.
    crate::boot::install_channel_table(table.scheduler());

    // Demonstrate the FULL cross-boundary channel handshake through the real
    // KernelSyscallEnv methods (request -> poll -> accept -> outcome -> send ->
    // recv) with two distinct boundaries, before running the lanes.
    demonstrate_cross_boundary_handshake();

    // Demonstrate the kernel Lattice (Gate/Link/Warden) over the same channel
    // substrate, through the real KernelSyscallEnv net methods.
    demonstrate_lattice();

    *RING3_TABLE.lock() = Some(table);

    // Publish the frame allocator so a ring-3 `spawn` syscall can map a new
    // lane's stack into the caller's space. Cleared after the run.
    crate::loader::set_spawn_frames(frames);

    // Switch to the lanes' space and run the cooperative selector loop against
    // the installed table. When a lane yields it parks on a short timer wait, so
    // the OTHER ready lane runs before the parked one is released (advance_clock)
    // and resumed — making the lane switch deterministic in the serial order.
    crate::loader::install_space(&space);
    let park_deadline = core::cell::Cell::new(1u64);
    run_installed(|_lane| {
        let d = park_deadline.get();
        park_deadline.set(d + 5);
        Some(WaitResource::Timer(Monotonic::from_millis(d + 5)))
    });

    // Back to the kernel's space; read exit codes from the installed table, then
    // clear it (dropping all lanes — safe now that no ctx pointers are in flight).
    crate::arch::paging::install(cibos_kernel::PhysFrame::containing(kernel_root));
    {
        let guard = RING3_TABLE.lock();
        if let Some(t) = guard.as_ref() {
            kprintln!(
                "  lane A exited (code {:?}), lane B exited (code {:?})",
                t.exit_code(a), t.exit_code(b)
            );
            // The child lane A spawned is lane #3 (after A=#1, B=#2). It was
            // spawned with arg 0x42 and exits with code = its arg (rdi). A 0x42
            // here PROVES the spawn arg was marshaled into the new lane's context.
            let child = LaneId(3);
            match t.exit_code(child) {
                Some(code) => kprintln!(
                    "  spawned child (lane #{}) exited with code {:#x} (== spawn arg -> arg marshaled)",
                    child.0, code
                ),
                None => kprintln!("  spawned child (lane #{}) did not record an exit", child.0),
            }
        }
    }
    *RING3_TABLE.lock() = None;
    crate::loader::clear_spawn_frames();
    crate::boot::clear_channel_table();
    kprintln!("CIBOS kernel: ring-3 multilane demo OK");
}

// ---- Cross-boundary channel handshake demo (feature: ring3-multilane-demo) --
//
// Proves the FULL cross-boundary IPC path through the REAL KernelSyscallEnv
// handshake methods (the exact code ring-3 dispatch reaches), with two distinct
// boundaries X=0x100 (requester) and Y=0x200 (target):
//   X.request_channel(Y, terms) -> request_id
//   Y.poll_channel_request()    -> sees X's request + terms
//   Y.accept_channel(id)        -> Y's handle (accept-ALL)
//   X.poll_channel_outcome(id)  -> X's handle (same kernel channel)
//   X.channel_send(handle, msg) ; Y.channel_recv(handle) -> msg
// The bytes pass THROUGH THE KERNEL (try_send copies in, try_recv copies out) —
// never via shared user memory. This is kernel-mediated, isolation-preserving
// cross-boundary IPC.
pub fn demonstrate_cross_boundary_handshake() {
    use cibos_kernel::syscall::SyscallEnv;
    use shared::protocols::ipc::{ChannelDirection, ChannelTerms};
    use shared::BoundaryId;

    kprintln!("CIBOS kernel: --- cross-boundary channel handshake demo ---");

    let env = crate::boot::kernel_syscall_env();
    let x = BoundaryId(0x100); // requester
    let y = BoundaryId(0x200); // target

    let terms = match ChannelTerms::new("xbdemo", ChannelDirection::Bidirectional, 64, 2) {
        Ok(t) => t,
        Err(_) => {
            kprintln!("  handshake demo: terms rejected — skipping");
            return;
        }
    };

    // X proposes a channel to Y. No channel exists yet.
    let req_id = match env.request_channel(x, y, &terms) {
        Ok(id) => {
            kprintln!("  X (0x100) requested a channel to Y (0x200): request #{}", id);
            id
        }
        Err(_) => {
            kprintln!("  handshake demo: request failed — skipping");
            return;
        }
    };

    // X polls its outcome BEFORE Y decides: must be pending (WouldBlock).
    match env.poll_channel_outcome(x, req_id) {
        Err(shared::protocols::syscall::SyscallError::WouldBlock) => {
            kprintln!("  X polled outcome early: still pending (correct)");
        }
        _ => kprintln!("  X early-poll: UNEXPECTED (should be pending)"),
    }

    // Y polls for pending requests aimed at it, and sees X's proposal + terms.
    let mut out = [0u8; shared::protocols::ipc::CHANNEL_REQUEST_WIRE_LEN];
    let polled = env.poll_channel_request(y, &mut out);
    match polled {
        Ok(id) => {
            let wire = shared::protocols::ipc::ChannelRequestWire::from_bytes(&out);
            kprintln!(
                "  Y polled: request #{} from boundary {:#x}, terms cap={} max_msg={}",
                id, wire.requester, wire.terms.buffer_capacity, wire.terms.max_message_bytes
            );
        }
        Err(_) => {
            kprintln!("  handshake demo: Y saw no request — skipping");
            return;
        }
    }

    // A WRONG boundary (0x999) must NOT be able to accept X's request (point-to-
    // point isolation): proves the target check.
    match env.accept_channel(BoundaryId(0x999), req_id) {
        Err(_) => kprintln!("  wrong boundary (0x999) accept REJECTED (correct isolation)"),
        Ok(_) => kprintln!("  wrong-boundary accept SUCCEEDED — ISOLATION BUG"),
    }

    // Y accepts WHOLESALE -> Y's handle.
    let y_handle = match env.accept_channel(y, req_id) {
        Ok(h) => {
            kprintln!("  Y accepted -> Y handle {}", h);
            h
        }
        Err(_) => {
            kprintln!("  handshake demo: Y accept failed — skipping");
            return;
        }
    };

    // X polls its outcome again -> now accepted, X's handle.
    let x_handle = match env.poll_channel_outcome(x, req_id) {
        Ok(h) => {
            kprintln!("  X polled outcome: accepted -> X handle {}", h);
            h
        }
        Err(_) => {
            kprintln!("  handshake demo: X never got its handle — skipping");
            return;
        }
    };

    // X sends bytes on its handle; Y receives them on its handle — SAME kernel
    // channel, bytes copied through the kernel across the boundary.
    match env.channel_send(x, x_handle, b"hello-Y") {
        Ok(()) => kprintln!("  X sent 'hello-Y' on handle {}", x_handle),
        Err(_) => kprintln!("  X send failed"),
    }
    match env.channel_recv(y, y_handle) {
        Ok(msg) => {
            if msg.as_slice() == b"hello-Y" {
                kprintln!("  Y received 'hello-Y' ({} bytes) — CROSS-BOUNDARY IPC OK", msg.len());
            } else {
                kprintln!("  Y received {} bytes (unexpected content)", msg.len());
            }
        }
        Err(_) => kprintln!("  Y recv failed"),
    }

    kprintln!("CIBOS kernel: cross-boundary handshake demo complete");
}

// ---- Lattice networking demo (feature: ring3-multilane-demo) ----------------
//
// Proves the kernel Lattice (Gate/Link/Warden) through the REAL KernelSyscallEnv
// net methods — the exact code ring-3 dispatch reaches — with two boundaries
// S=0x300 (server, binds a Gate) and C=0x400 (client, connects):
//   S.gate_bind(80)      -> listener
//   C.gate_connect(80)   -> client Link
//   S.gate_accept(80)    -> server Link (the other half of ONE Channel)
//   C.link_send / S.link_recv  -> bytes cross boundaries through the kernel
//   S.link_send / C.link_recv  -> reply path (bidirectional)
// Plus: a Warden-denied Gate refuses BOTH bind and connect (total denial), and a
// probe reports Open/Closed/Blocked. Loopback-backed; the SAME surface a NIC
// transport will sit beneath (NETWORKING.md).
pub fn demonstrate_lattice() {
    use cibos_kernel::syscall::SyscallEnv;
    use shared::BoundaryId;

    kprintln!("");
    kprintln!("CIBOS kernel: --- Lattice networking demo (Gate/Link/Warden) ---");

    let env = crate::boot::kernel_syscall_env();
    let s = BoundaryId(0x300); // server (binds)
    let c = BoundaryId(0x400); // client (connects)
    let gate: u16 = 80;

    // Warden denial is TOTAL: deny gate 81, prove bind AND connect are refused.
    let denied: u16 = 81;
    let _ = env.warden_set(s, denied, false);
    match env.gate_bind(s, denied) {
        Err(_) => kprintln!("  Warden-denied gate {denied}: bind REFUSED (correct)"),
        Ok(_) => kprintln!("  Warden-denied gate {denied}: bind SUCCEEDED — WARDEN BUG"),
    }
    match env.gate_connect(c, denied) {
        Err(_) => kprintln!("  Warden-denied gate {denied}: connect REFUSED (correct)"),
        Ok(_) => kprintln!("  Warden-denied gate {denied}: connect SUCCEEDED — WARDEN BUG"),
    }

    // Probe the (allowed, unbound) service gate -> Closed.
    match env.gate_probe(c, gate) {
        Ok(0) => kprintln!("  probe gate {gate}: Closed (allowed, unbound)"),
        Ok(other) => kprintln!("  probe gate {gate}: state {other} (expected Closed)"),
        Err(_) => kprintln!("  probe failed"),
    }

    // S binds the service gate.
    match env.gate_bind(s, gate) {
        Ok(_) => kprintln!("  S (0x300) bound gate {gate}"),
        Err(_) => {
            kprintln!("  S bind failed — skipping");
            return;
        }
    }

    // Now a probe reports Open.
    if let Ok(1) = env.gate_probe(c, gate) {
        kprintln!("  probe gate {gate}: Open (listener bound)");
    }

    // C connects -> client Link handle.
    let c_link = match env.gate_connect(c, gate) {
        Ok(h) => {
            kprintln!("  C (0x400) connected to gate {gate} -> client link {h}");
            h
        }
        Err(_) => {
            kprintln!("  C connect failed — skipping");
            return;
        }
    };

    // S accepts the pending connect -> server Link handle (other half).
    let s_link = match env.gate_accept(s, gate) {
        Ok(h) => {
            kprintln!("  S accepted -> server link {h}");
            h
        }
        Err(_) => {
            kprintln!("  S accept failed — skipping");
            return;
        }
    };

    // C -> S: client sends, server receives (bytes through the kernel).
    match env.link_send(c, c_link, b"GET /") {
        Ok(()) => kprintln!("  C sent 'GET /' on client link"),
        Err(_) => kprintln!("  C send failed"),
    }
    match env.link_recv(s, s_link) {
        Ok(msg) if msg.as_slice() == b"GET /" => {
            kprintln!("  S received 'GET /' ({} bytes) — LATTICE LINK OK", msg.len())
        }
        Ok(msg) => kprintln!("  S received {} bytes (unexpected)", msg.len()),
        Err(_) => kprintln!("  S recv failed"),
    }

    // S -> C: server replies, client receives (bidirectional Link).
    match env.link_send(s, s_link, b"200 OK") {
        Ok(()) => kprintln!("  S replied '200 OK' on server link"),
        Err(_) => kprintln!("  S send failed"),
    }
    match env.link_recv(c, c_link) {
        Ok(msg) if msg.as_slice() == b"200 OK" => {
            kprintln!("  C received '200 OK' ({} bytes) — BIDIRECTIONAL OK", msg.len())
        }
        Ok(msg) => kprintln!("  C received {} bytes (unexpected)", msg.len()),
        Err(_) => kprintln!("  C recv failed"),
    }

    kprintln!("CIBOS kernel: Lattice networking demo complete");
}
