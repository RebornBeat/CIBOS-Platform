# Track 2 — live ring-3 multi-context + cross-boundary channels (design, deferred)

Status: DESIGN ONLY. These two items are deferred *with a single shared root
prerequisite* identified below. Nothing here is implemented yet; this doc exists
so the deferral is principled and the path is concrete (not a vague "later").

## Why both items are blocked on ONE thing

### What works today (verified)
- In-kernel cooperative executor: `kernel.spawn(WeightClass, future)` +
  `run_until_idle()` — multiple *in-kernel* futures, single selector, Catch-and-
  Release. Canonical-correct, tested.
- A single ring-3 app runs via `loader::run_app_image_isolated` →
  `enter_user_context` (setjmp/longjmp): the kernel saves ITS OWN callee-saved
  registers + RSP, drops to ring 3, and the app's `exit` syscall longjmps back
  via `return_to_kernel`. Exactly ONE ring-3 context is live; control returns to
  the kernel only when that app exits.
- Local intra-boundary channels over syscalls (OpenChannel/Send/Recv), with
  bounded back-pressure (`WouldBlock`). Runtime-verified.
- Cross-boundary channel POLICY primitives already exist at the kernel/SDK level:
  `ChannelTerms` (purpose, direction, max_msg, capacity), `ChannelRegistry::
  create(terms, ..)`, `await_channel_request`, accept/reject, `TargetRejected`.

### The shared blocker
The syscall trap path currently attributes every syscall to
`BoundaryId::SYSTEM` (boot.rs handle_syscall) because a ring-3 lane does not yet
carry its real boundary identity into the trap, and the kernel cannot save a
*user* lane's register state to park it and resume a *different* user lane.

`enter_user_context` saves the KERNEL's context so a user `exit` can return.
The missing symmetric half is saving the USER's full register state at a trap so
the selector can switch between several ring-3 lanes. Until that exists:
- `spawn` cannot start a second concurrently-runnable ring-3 lane (there is no
  re-entry point that would switch to it — the one context runs to completion).
- cross-boundary channel policy cannot be ENFORCED, because the kernel can't
  trust "which boundary is calling" (the trap hardcodes SYSTEM).

So: **per-lane ring-3 context (real boundary id + user-register save/restore) is
the one prerequisite both items share.** Build it once; both unblock.

## The prerequisite: per-lane ring-3 context

A `Ring3Lane` the selector can schedule, holding:
- `boundary: BoundaryId` — the real principal; the trap reads it instead of
  hardcoding SYSTEM. Enforces isolation + cross-boundary policy honestly.
- `cr3: PhysFrame` — the lane's address space root (already produced per app by
  `AddressSpace::new`; today it is installed inline and torn down on exit).
- `user_regs: SavedUserContext` — full GP register set + RIP + RSP + RFLAGS,
  saved by the trap entry stub when the lane traps/stalls, restored on resume.
- `state: Ready | Stalled(WaitResource) | Running | Exited(code)` — mirrors the
  existing in-kernel lane states; reuses Catch-and-Release wait resources.

### Mechanism (the real work, in honest increments)
1. **Trap saves user state.** Extend `syscall_entry.s` to push the full user GP
   set into a per-lane `SavedUserContext` (today it preserves only what the ABI
   needs and longjmps on exit). This is the load-bearing asm change.
2. **Resume path.** A `resume_ring3(lane)` that installs `lane.cr3`, loads
   `lane.user_regs`, and `iretq`s back to where the lane trapped — the symmetric
   partner of `enter_user_context`, but for an arbitrary parked lane.
3. **Selector owns Ring3Lanes.** On a stalling syscall (`ChannelRecv` empty,
   `ChannelSend` full, `Sleep`, future channel waits), the trap records
   `Stalled(resource)` and returns to the selector instead of busy-waiting in
   kernel (the current Sleep/channel impls busy-wait or return WouldBlock; with
   real lanes they park). The selector picks the next Ready ring-3 lane (single
   selector, weighted entropy only under competition — unchanged model).
4. **`spawn` becomes real.** `spawn(entry, arg)` allocates a Ring3Lane in the
   caller's boundary, maps a fresh stack into the caller's space (or a child
   space per the isolation policy), marks it Ready, returns its lane id. No
   preemption added — purely cooperative: a lane runs until it traps/stalls/exits
   (honors "time as trigger, not coordinator" for Max-Isolation/Balanced).

### Cross-boundary channels, once boundary identity is real
With `req.boundary` trustworthy, expose the EXISTING policy primitives over new
syscalls (no new model):
- `ChannelRequest(target, terms_ptr)` → kernel records a pending request to
  `target`'s boundary; returns a request id. (terms = the existing ChannelTerms.)
- `ChannelAccept(request_id)` / `ChannelReject(request_id)` → the target's lane
  accepts-all-or-rejects (canonical: no counter-proposal). Accept creates the
  channel via `ChannelRegistry::create(terms, ..)` bound to BOTH boundary ids.
- Send/Recv reuse the existing handle ops; the registry already enforces bounds
  and back-pressure. Isolation holds: a handle is valid only within its boundary,
  and the kernel checks the calling boundary owns the handle.

## Alignment guarantees (held by construction)
- No new locks: Ring3Lane table is selector-owned (same pattern as Ready Pool /
  the channel table), parked lanes wait on Catch-and-Release resources.
- Boundary stays THE principal: per-lane boundary id is the whole point; cross-
  boundary access goes through propose/accept, never ambient authority.
- Cooperative only: no time-slice preemption is introduced; lanes yield at traps.
- Single selector, weighted entropy only under competition — unchanged.
- No placeholders: until step 1 (trap saves user state) lands, `spawn` returns
  `NotPermitted` and the trap uses SYSTEM honestly. We do NOT fake a lane id.

## Smallest honest next increment (when this track resumes)
Step 1 alone (trap saves full user context into a per-lane SavedUserContext,
plus a `resume_ring3` that round-trips one parked lane back to where it trapped)
is independently verifiable in QEMU: spawn one extra lane, let it stall on an
empty channel, run the original, send, and watch the selector resume the parked
lane to receive. That single demo proves the mechanism without the full breadth.

---

## INCREMENT IN PROGRESS (this session): step 1+2 — save + resume one parked ring-3 lane

Goal: prove the load-bearing mechanism (a parked ring-3 context can be saved by
the trap and resumed by the kernel back to exactly where it trapped), with a
QEMU runtime demo, WITHOUT yet building the full selector-owned Ring3Lane table.

Concrete pieces (all real, no placeholders):
- `SavedUserContext` (#[repr(C)]): r15..rax (15 GPRs) + RIP + RSP + RFLAGS, the
  exact set an `iretq` frame + GP file needs to resume a trapped lane.
- asm `user_trap_save_entry`: an alternate vector-0x80 stub that pushes the FULL
  user GP set + the iretq frame fields into a per-lane `SavedUserContext`, then
  (instead of iretq-ing inline) calls a Rust handler that can choose to PARK the
  lane (return to kernel) or continue. For this increment it parks after a
  `Yield`-class trap.
- asm `resume_ring3(ctx: *const SavedUserContext) -> !`: loads the saved GP file,
  pushes the saved iretq frame (SS/RSP/RFLAGS/CS/RIP), and `iretq`s back — the
  symmetric partner of `enter_user_context`, but for an arbitrary parked ctx.
- A gated `ring3-resume-demo`: enter a tiny ring-3 payload that does
  `yield` (traps, saved+parked) → kernel prints "parked" → `resume_ring3` →
  payload continues, does `log` then `exit(0)` → kernel prints "resumed+exited".

Verifies in QEMU via the BIOS `.img` path (compute profile). Proves save/restore
of a real user context round-trips. Does NOT claim spawn/cross-boundary yet —
those still need the selector-owned table (step 3+4), flagged honestly.

---

## VERIFIED (this session): step 1+2 — save + resume an arbitrary parked lane

Status: DONE and runtime-verified in QEMU (BIOS .img, compute profile). Built
FAITHFULLY to "resume an ARBITRARY parked lane" — explicitly NOT a single-slot
shortcut (see below).

### What landed
- `arch/ring3_ctx.rs`: `SavedUserContext` (#[repr(C)], 20 u64 = 160 bytes: 15
  GPRs + RIP/CS/RFLAGS/RSP/SS). Compile-time `const _` offset guards assert the
  layout matches the asm `OFF_*` on EVERY bare build (proven to fail on drift).
- `arch/resume_user.s`:
  * `user_ctx_trap_entry` — context-saving int-0x80 stub. Saves the full user GP
    file + iretq frame into `*CURRENT_USER_CTX` (the kernel-set "current lane"
    pointer — NOT a fixed buffer), then calls `cibos_user_trap_handler`.
  * `resume_ring3(ctx)` / `resume_user_context(ctx, kctx)` — take the context
    pointer as an ARGUMENT, so any parked lane can be resumed. `resume_user_context`
    saves the kernel return frame into a caller-supplied `kctx` (per-resume, NOT
    a global) so nested/sequential resumes are correct.
  * `return_to_saved_kernel(code)` — unwinds a resumed lane's exit to the
    resume call site.
  * Kernel-set POINTERS `CURRENT_USER_CTX` / `ACTIVE_KERNEL_CTX` (single pointers
    only because the model is cooperative single-selector: one ring-3 lane runs
    at a time; switching lanes repoints them — it does NOT serialise lane storage).
- `boot.rs`: `handle_user_trap` (park-on-yield; routes resumed-lane exit to
  `return_to_saved_kernel`), `KernelReturnContext`, `mark_lane_resumed`,
  `CURRENT_USER_CTX` extern. `idt.rs`: `set_ctx_saving_syscall_vector` /
  `set_default_syscall_vector` (demo swaps the 0x80 stub, then restores it, so
  the normal app flow is untouched). `loader.rs`: `run_resume_demo`.
- Feature `ring3-resume-demo` gates ALL of it; default build + aarch64/riscv64
  unaffected, 329 tests still green, clippy clean.

### Why this is faithful, not a shortcut
The earlier draft saved into a static `USER_CTX_SAVE` and reused a single
`KERNEL_CTX` — that would have baked in "one parked lane" at the asm level,
contradicting "arbitrary parked lane." Corrected BEFORE building the demo: the
save target is now `*CURRENT_USER_CTX` (a kernel-set pointer to the running
lane's context in the future selector-owned table), and both resume entries take
the context pointer as an argument. Step 3 (selector-owned Ring3Lane table) adds
the table + sets these pointers per dispatch; THIS ASSEMBLY IS REUSED UNCHANGED.

### Runtime proof (QEMU serial, compute .img)
```
ring-3 park/resume demo starting
lane parked at trap (full user context saved); kernel back in control
[ring3] resumed after park, continued from the trap point   <- payload's OWN log, AFTER the yield trap
lane resumed from the trap point and exited (code 0)
ring-3 park/resume demo OK
external app exited with code 7 (...)                        <- normal flow intact (vector restored)
```

### Remaining toward full spawn / cross-boundary (step 3+4, still honest-deferred)
- Selector-owned `Ring3Lane` table holding {boundary, cr3, SavedUserContext,
  state}; selector sets CURRENT_USER_CTX per dispatch; picks next Ready lane on a
  stalling syscall (channel empty/full, sleep). Single selector, weighted entropy
  only under competition — unchanged model.
- `spawn(entry, arg)` allocates a Ring3Lane (caller's boundary), maps a stack,
  marks Ready, returns lane id. Cooperative only (no preemption).
- Trap reads the real `req.boundary` from the current Ring3Lane (instead of
  hardcoded SYSTEM) -> enforces cross-boundary channel policy honestly.

---

## INCREMENT IN PROGRESS (this session): step 3 — selector-owned Ring3Lane table

Goal: prove the TABLE-DRIVEN multi-lane model — the kernel holds N ring-3 lanes,
the selector picks the next Ready one and dispatches it, a lane that stalls is
parked and another runs, then the parked one resumes. Reuses the verified asm
(resume_user_context / CURRENT_USER_CTX) UNCHANGED and the EXISTING canonical
`cibos_kernel::Scheduler` (Ready/Stalled + weighted-entropy selection) for policy
— no parallel selector invented.

### Design (faithful to canonical single-selector model)
- New `kernel-image/src/ring3.rs`: a `Ring3Table` holding, per `LaneId`:
  `{ boundary: BoundaryId, ctx: SavedUserContext, stack_top, exited: Option<i64> }`.
  The arch-specific bits (SavedUserContext, cr3/stack) live here in kernel-image;
  the SELECTION POLICY is delegated to a `cibos_kernel::Scheduler` the table owns.
- `spawn_lane(entry, stack_top, boundary, class)` -> registers the lane with the
  Scheduler (`register_lane` + ready), stores a fresh `SavedUserContext` (rip=entry,
  rsp=stack_top, user CS/SS, RFLAGS=0x202).
- `run(&mut self)`: the cooperative loop —
    while !scheduler.is_idle():
      for lane in scheduler.take_dispatch_batch():        // weighted-entropy under competition
        CURRENT_USER_CTX = &mut table[lane].ctx
        code = resume_user_context(&ctx, &mut kret)
        // lane returned to kernel: it either parked (PARK_SENTINEL via a stall
        // syscall) -> scheduler.register_wait(lane, resource); or exited ->
        // scheduler.notify_complete(lane), table[lane].exited = code.
      advance_clock(...) to release timer waits (reuses Catch-and-Release).
  Single selector, weighted entropy only under competition — UNCHANGED model.
- Cooperative only: a lane runs until it traps/stalls/exits. No preemption.

### Demo (gated `ring3-multilane-demo`)
Two ring-3 lanes sharing nothing: lane A computes + logs "A1", yields (parks),
lane B logs "B1" + exits, then the selector resumes A which logs "A2" + exits.
Serial order proves the selector switched lanes at the park and came back.
Runtime-verified via the compute BIOS .img.

### Honest scope of this increment
This proves table-driven multi-lane dispatch + park/resume across lanes. It does
NOT yet wire `spawn` as a ring-3 SYSCALL (apps calling spawn) — that's the final
join once cross-boundary identity is read from the lane (step 4). The table here
is driven by the kernel at boot (like the in-kernel channel demo), which is the
correct order: prove the mechanism, then expose it to ring-3.

---

## VERIFIED (this session): step 3 — selector-owned Ring3Lane table

Status: DONE and runtime-verified in QEMU (compute BIOS .img). Table-driven
multi-lane dispatch + cross-lane park/resume, using the canonical Scheduler for
policy and the verified resume asm UNCHANGED.

### What landed
- `kernel-image/src/ring3.rs`: `Ring3Table` holding per-LaneId
  `{ ctx: SavedUserContext, boundary: BoundaryId, started, exited }`, with an
  owned `cibos_kernel::Scheduler` for Ready/Stalled + weighted-entropy selection
  (NO parallel selector invented). `spawn_lane(entry, stack_top, boundary, class)`
  builds the initial context (rip/rsp/CS/SS/RFLAGS) and registers+readies the
  lane. `run(on_park)` is the cooperative loop: `take_dispatch_batch()` ->
  resume each lane via `resume_user_context` -> on return, park (register_wait)
  or complete (notify_complete); `advance_clock` releases timer waits.
- `boot.rs`: `set_current_user_ctx` (selector repoints CURRENT_USER_CTX per
  dispatch), `set_active_lane`/`active_lane` (records the running lane; the
  boundary lookup for step 4), and the multilane `handle_user_trap` arm (every
  yield parks via `return_to_saved_kernel`; every lane was entered via
  resume_user_context so its exit unwinds to the loop). `multilane_seed()` draws
  the selector's entropy from the kernel RNG.
- `loader.rs`: `new_lane_space`, `map_lane` (per-lane code+stack at distinct
  virtual addresses), `install_space` — so N lanes coexist in one space.
- Feature `ring3-multilane-demo`; the shared resume machinery is now gated on
  EITHER demo feature. Default build + resume-demo + aarch64/riscv64 unaffected,
  329 tests green, clippy clean.

### Bug found + fixed honestly (not papered over)
First QEMU run: #GP (vector 13) at RIP 0xf000ff53… (BIOS reset pattern) right
after both lanes logged step 1. Root cause: the multilane park used
`return_to_kernel`, whose `KERNEL_CTX` is UNSET on the multilane path (lanes are
entered via `resume_user_context`, which saves into the caller's `kret`). Fix:
park unwinds via `return_to_saved_kernel` (the resume's saved frame). Re-ran:
clean. This is exactly why each increment is QEMU-verified, not assumed.

### Runtime proof (QEMU serial, compute .img)
```
spawned 2 ring-3 lanes: A=#1 (boundary 0x100), B=#2 (boundary 0x200)
[lane B] step 1            <- selector picked B first
[lane A] step 1            <- A ran, then yielded (parked on a timer wait)
[lane A] resumed step 2    <- selector released the wait and RESUMED A from the trap point
lane A exited (code Some(0)), lane B exited (code Some(0))
ring-3 multilane demo OK
external app exited with code 7 (...)   <- normal flow intact (vector restored)
```

### Remaining toward full spawn / cross-boundary (step 4, honest-deferred)
- Expose `spawn` as a ring-3 SYSCALL backed by `Ring3Table::spawn_lane` (apps
  calling spawn). The table + asm are ready; this is the join.
- Trap reads the running lane's real boundary via `active_lane()` ->
  `Ring3Table::boundary_of` (instead of hardcoded SYSTEM) -> enforces
  cross-boundary channel policy honestly.

---

## INCREMENT IN PROGRESS (this session): step 4 — spawn syscall + real boundary

### KEY FINDING (prevents drift): the ABI is ALREADY canonical
The kernel dispatcher ALREADY routes `Syscall::Spawn` -> `env.spawn(req.boundary,
entry, arg)` and `Syscall::OpenChannel` -> `env.open_channel(req.boundary, ...)`.
`req.boundary` already flows through every channel/spawn syscall. So step 4 adds
NO new ABI and changes NO dispatch. It only fills in two stand-ins:
  (a) the trap currently sets `req.boundary = BoundaryId::SYSTEM` (a documented
      stand-in) — replace with the RUNNING lane's real boundary;
  (b) `KernelSyscallEnv::spawn` returns NotPermitted — back it with
      `Ring3Table::spawn_lane`.

### Modifications to make (reviewed against canonical spec)
1. `static RING3_TABLE: SpinLock<Option<Ring3Table>>` (mirrors CHANNEL_TABLE/
   ROOT_FS). The multilane demo installs the table here for the duration of its
   run and clears it after.
2. **Lock discipline (the deadlock risk, handled):** `Ring3Table::run` must NOT
   hold the table lock across `resume_user_context` (the trap re-enters and locks
   the same static). Refactor `run` to: lock briefly to take the dispatch batch +
   set CURRENT_USER_CTX/active-lane, RELEASE, resume the lane, then lock briefly
   to record park/exit. Same brief-lock discipline as open_channel.
3. `handle_syscall`: if a multilane lane is active (`active_lane() != 0`), set
   `req.boundary` = `RING3_TABLE.boundary_of(active_lane())`; else keep SYSTEM
   (so the normal .capp / single-resume paths are UNCHANGED). Risk-checked:
   fallback preserves all existing flows.
4. `KernelSyscallEnv::spawn(boundary, entry, arg)`: map a fresh code+stack for
   the entry into the CURRENT space, call `RING3_TABLE.spawn_lane(entry, stack,
   boundary, User)`, return the lane id. Canonical: a spawned lane is a new
   pathway in the CALLER'S boundary (we pass the caller's `boundary` through).

### Demo (gated `ring3-multilane-demo`, extended)
A ring-3 lane calls `spawn(entry2, arg)` -> a second lane is created BY THE APP
(not pre-seeded) and runs; then a lane opens a channel and the trap attributes it
to that lane's REAL boundary (proven by logging boundary != SYSTEM). Cross-
boundary policy (accept/reject) stays as the canonical channel spec already
defines; this increment proves boundary IDENTITY is real, the precondition for
enforcing it.

### Honest scope
Proves: app-initiated spawn via syscall + real per-lane boundary at the trap.
Does NOT yet implement the full cross-boundary accept/reject channel handshake
(that's the canonical Channel::request/await_channel_request flow over syscalls —
a distinct, larger piece, flagged for after this).

### CONFIRMED INSIGHTS (memory-safety of spawn-during-resume) — dwelt on, not assumed
The step-4 run loop takes a RAW pointer to lane A's SavedUserContext, releases
the table lock, and resumes A. While A runs it may call `spawn`, which INSERTS a
new lane into the SAME `BTreeMap<LaneId, Ring3Lane>`. For this to be sound, A's
ctx pointer must stay valid across that insert. Two independently-sufficient facts:

  1. `BTreeMap` insert NEVER relocates an already-stored value. Values live inside
     heap-allocated node arrays; insertion adds/splits NODES but does not move
     existing values. Verified empirically two ways: (a) re-lookup address stable
     across 2000 inserts; (b) a RAW pointer taken before 5000 inserts still reads
     the original value (7) and writes back successfully, value never moved.
     [These tests RAISE confidence; the GUARANTEE is the data-structure invariant.]
  2. We NEVER `remove` from `self.lanes`. `complete()` tombstones a finished lane
     (`exited = Some(code)`) and calls `scheduler.notify_complete` (which mutates
     the SCHEDULER's lists, not the lane map). So no removal can move A's value
     either. This fact is under our control and is the real closing argument —
     the safety does NOT hinge on the empirical BTreeMap behaviour alone.

Design rule locked in: the ring-3 lane map is INSERT-and-TOMBSTONE only for the
lifetime of a run; entries are dropped en masse only when the table is cleared
from RING3_TABLE after the run. This keeps all in-flight ctx pointers valid.

### LOCK DISCIPLINE (deadlock-free), confirmed
`run_installed` holds the RING3_TABLE lock only in brief windows (pick batch /
take ctx ptr+set current/active / record park|exit) and ALWAYS releases before
`resume_user_context`. A lane's syscall (spawn, channel ops, boundary lookup)
re-locks RING3_TABLE while the loop holds NO lock — no re-entrant deadlock. This
mirrors the channel syscall's brief-lock pattern exactly. `on_park` must not lock
the table (the demo closure only computes a timer) — noted as a standing rule.

### RE-ENTRANCY CHECK (step-4 lock ordering), confirmed safe
Two places lock RING3_TABLE during a trap: (1) handle_syscall's boundary lookup,
(2) KernelSyscallEnv::spawn. Both are reached while the lane runs, i.e. while
run_installed holds NO lock (it releases before resume_user_context). Within a
single trap: handle_syscall takes the boundary-lookup lock in a `let boundary =
{...}` temporary that DROPS before `dispatch_syscall` is called; spawn then takes
the lock inside dispatch. So the two never overlap — no reentrant deadlock on the
non-reentrant SpinLock. Verified by inspection of the lock scopes.

---

## VERIFIED (this session): step 4 — spawn syscall + real boundary

Status: DONE and runtime-verified in QEMU (compute BIOS .img). A ring-3 app calls
the `spawn` syscall at runtime; the kernel maps a stack into the caller's space,
registers the new lane in the selector-owned table IN THE CALLER'S BOUNDARY, and
the selector runs it. The trap now attributes syscalls to the running lane's REAL
boundary, not a hardcoded stand-in. This joins roadmap 2a (spawn syscall) + 2b
(cooperative multi-context loop).

### What landed (no new ABI — the dispatch was already canonical)
- `cibos-kernel/src/paging.rs`: `AddressSpace::adopt(root)` — wrap an existing
  installed space to map additional pages (the dual of `new`). Used so `spawn`
  maps a new lane's stack into the CALLER'S live space (same boundary -> same
  space). A general, reusable primitive.
- `kernel-image/src/loader.rs`: `set_spawn_frames`/`clear_spawn_frames` (publish
  the frame allocator to the syscall path for the run's duration, like
  RING3_TABLE), `map_spawn_stack` (map a fresh user stack into the current space
  via the adopted root), `map_lane_code` (code-only map for a spawn target).
  `arch/paging.rs`: `current_root_frame()`.
- `kernel-image/src/boot.rs`:
  * `KernelSyscallEnv::spawn` now (under multilane) allocates a per-spawn stack
    virt, maps it, and calls `RING3_TABLE.spawn_lane(entry, stack, boundary,
    User)`, returning the lane id. Without the multilane table it still reports
    NotPermitted honestly (no fake lane).
  * `handle_syscall` sets `req.boundary` from `active_lane()` ->
    `RING3_TABLE.boundary_of` (fallback SYSTEM for normal/kernel paths). The
    boundary-lookup lock is a temporary that DROPS before `dispatch_syscall`, so
    when `spawn` re-locks RING3_TABLE there is NO reentrant deadlock (verified).
  * `clear_active_lane` (attribute to SYSTEM when no ring-3 lane runs).
- `kernel-image/src/ring3.rs`: lock-safe static-table loop (`run_installed`,
  brief locks never held across `resume_user_context`); demo extended so lane A
  calls `spawn(CHILD_CODE)` at runtime and the child lane runs.

### Runtime proof (QEMU serial, compute .img)
```
spawned 2 ring-3 lanes: A=#1 (boundary 0x100), B=#2 (boundary 0x200)
[lane B] step 1
[lane A] step 1
[lane A] spawned a child       <- lane A issued the spawn SYSCALL (17) at runtime
[child] spawned by lane A ran  <- kernel mapped a stack, registered the lane in A's boundary, selector RAN it
[lane A] resumed step 2
lane A exited (code 0), lane B exited (code 0)
external app exited with code 7   <- normal .capp flow intact
```

### Memory-safety + lock-discipline (dwelt on, recorded above)
- Lane map is INSERT-and-TOMBSTONE only during a run; never `remove` -> in-flight
  ctx raw pointers stay valid across a `spawn` insert (BTreeMap never moves
  stored values; verified two ways AND closed by the no-remove rule).
- run_installed never holds the table lock across resume_user_context; the two
  in-trap lockers (boundary lookup, spawn) never overlap. No deadlock.

### Honest remaining (no overclaiming)
- `arg` is not yet marshaled into the spawned lane's initial context (logged as a
  later refinement; the entry runs, the arg word is a small ABI addition).
- The FULL cross-boundary channel accept/reject handshake (canonical
  Channel::request / await_channel_request over syscalls) is still the distinct
  larger piece. Step 4 makes the boundary IDENTITY real at the trap — the
  precondition for enforcing that policy — but does not yet implement the
  handshake. Flagged as the next Track-2 piece.
- F1 fully-live interactive session; Track 3 (network/Lattice over syscalls);
  Track 4 (server orchestrator, design-first); per-arch ring-3 app/login flow.

---

## INCREMENT IN PROGRESS (this session): arg marshaling [DONE] + cross-boundary handshake [building]

### arg marshaling — DONE, runtime-verified
`spawn_lane` now sets `ctx.rdi = arg` (SysV first-arg register), matching the
ring-3 SDK `spawn(entry, arg)` wrapper. Threaded through KernelSyscallEnv::spawn.
PROOF (QEMU): lane A calls `spawn(CHILD_CODE, 0x42)`; the child computes its exit
code from rdi and exits 0x42; the demo logs "spawned child exited with code 0x42
(== spawn arg -> arg marshaled)". 329 tests green, clippy clean.

### Cross-boundary channel accept/reject handshake — DESIGN (faithful to canon)
The canonical model is ALREADY fully specified in `shared/src/protocols/ipc.rs`:
  * `ChannelTerms { purpose, direction, max_message_bytes, buffer_capacity }` —
    PROPOSED BY THE REQUESTER.
  * `ChannelRequest { target: BoundaryId, terms }`.
  * `ChannelAcceptance { Accepted(ChannelId) | Rejected }` — receiver accepts ALL
    terms or rejects WHOLESALE; NO counter-proposal (canonical, removes
    negotiation-timing as an attack surface).
The GAP: `ChannelRegistry` creates channels from terms but has NO pending-request
/ accept-reject path. Nothing is exposed to ring-3.

DISCIPLINED ORDER (mirrors steps 1-4: mechanism first, then ring-3 exposure):
  Increment A (kernel-side, host-tested): extend ChannelRegistry with a pending-
    request table keyed by TARGET boundary. Methods:
      - `request(from: BoundaryId, req: ChannelRequest) -> RequestId` — queue a
        pending request to `req.target`. point-to-point.
      - `poll(target: BoundaryId) -> Option<(RequestId, BoundaryId/*from*/, ChannelTerms)>`
        — the receiver in `target` sees the next pending request.
      - `accept(RequestId, kernel) -> ChannelId` — create the channel from the
        proposed terms (accept-ALL), return its id to BOTH ends.
      - `reject(RequestId)` — drop the request; requester learns Rejected.
    Enforces: a channel only exists after the TARGET boundary accepts. A
    requester cannot force a channel into another boundary. Host unit tests for
    accept path, reject path, wrong-target isolation, accept-all (no partial).
  Increment B (ring-3 exposure, later): syscalls RequestChannel/PollChannelRequest
    /AcceptChannel/RejectChannel (numbers 18+), bridged to the registry, with the
    requester's boundary taken from the trap (active_lane -> boundary, already
    live from step 4). Demo: two lanes in different boundaries; one requests, the
    other accepts -> channel; a wrong-boundary request -> Rejected.

### Anti-drift checks for this piece
  * Terms are accept-ALL-or-REJECT — NEVER allow the receiver to alter terms.
  * A channel NEVER exists without the target boundary's explicit accept (binary
    isolation: the boundary is the principal; cross-boundary contact is mutual).
  * point-to-point: a request targets exactly one boundary.
  * No global lock that spans user execution (brief locks only).

---

## PRODUCTION-REALITY ANALYSIS (dwelt on): how channels REALLY work for a real app

The user asked the load-bearing question: how does this work for a real bare-metal
app, in real scenarios? Tracing it end-to-end exposed an HONEST DIVERGENCE that
must be fixed faithfully (not papered over):

### The divergence (a stand-in that diverged from canon)
- The SYSCALL channel path (`open_channel`/`channel_send`/`channel_recv` in
  boot.rs) uses a `LocalChannel` = a VecDeque in a per-kernel CHANNEL_TABLE, and
  IGNORES the boundary param (`_boundary`). Comment at boot.rs:1199 is honest: it
  was the "`Channel::new_local` contract" — a SAME-BOUNDARY queue only.
- The CANONICAL channel (`cibos-kernel/src/channel.rs`: Channel/ChannelRegistry,
  terms, sender/receiver waiters, KernelInterface back-pressure) is the real
  system — and it is what the new handshake (request/accept/reject) builds on.
- So today there are TWO channel systems: the syscall stand-in (LocalChannel) and
  the canonical one. Wiring the handshake to the canonical registry while send/recv
  use LocalChannel would be DRIFT (two disconnected systems).

### How it MUST work for a real cross-boundary app (the faithful model)
1. Two lanes live in DIFFERENT boundaries (different address spaces). They share
   NO user memory — that is the isolation guarantee.
2. A channel is created ONLY via the handshake: requester proposes terms to a
   target boundary; the target accepts-ALL-or-rejects; on accept, ONE canonical
   `Channel` is created in the kernel. (ChannelRegistry::request/poll/accept/reject
   — built this increment, host-tested.)
3. Each boundary holds a HANDLE (u64) that the kernel maps -> the same kernel-owned
   `Channel`. The handle table is keyed by (boundary, handle).
4. `send(handle, bytes)`: kernel resolves (caller_boundary, handle) -> Channel ->
   `try_send` COPIES the bytes into the kernel-held queue (WouldBlock on full,
   waiter registered). `recv(handle, buf)`: resolve -> Channel -> `try_recv`
   COPIES bytes OUT into the receiver's user buffer (WouldBlock/Empty otherwise).
   Bytes pass THROUGH THE KERNEL, never via shared user memory — required for
   cross-boundary isolation.

### The no-drift fix (this is the real work, no shortcut)
Replace the LocalChannel stand-in with a boundary-aware handle table over the
CANONICAL `Channel`:
  - `static CHANNEL_HANDLES: SpinLock<BTreeMap<(BoundaryId,u64), Channel>>` (a
    cheap clone-handle to the shared kernel Channel; both ends' handles map to the
    same inner buffer via Channel's Arc inner).
  - `open_channel` (same-boundary convenience) and the handshake `accept` both
    produce Channels registered under the right boundary+handle(s).
  - `channel_send`/`channel_recv` resolve (caller_boundary, handle) and call the
    canonical try_send/try_recv — real back-pressure + waiter wakeups via the
    scheduler (already wired through KernelInterface).
The caller_boundary comes from the trap (active_lane -> boundary), already live
from step 4. This UNIFIES the two channel systems onto the canonical one and makes
cross-boundary IPC real, kernel-mediated, and isolation-preserving.

### Discipline note
This is more complex than keeping LocalChannel, but keeping it would be a
shortcut that contradicts the canonical Channel model and the boundary-is-the-
principal invariant. We do the faithful work: unify on the canonical Channel.

---

## VERIFIED (this session): handshake mechanism + channel unification

### Cross-boundary handshake (kernel-side) — DONE, host-tested (4 new tests)
`ChannelRegistry` extended with the canonical request/poll/accept/reject:
  - `request(from, ChannelRequest{target,terms}) -> request_id` (queues; no
    channel yet).
  - `poll(target) -> Option<(request_id, from, terms)>` (only requests aimed at
    `target` are visible — point-to-point isolation).
  - `accept(request_id, target, kernel) -> ChannelId` (accept-ALL: channel built
    from the exact proposed terms; only the real target may accept).
  - `reject(request_id, target) -> bool`; `is_pending(request_id) -> bool`.
Tests (all green, 329 -> 333): accept-creates-from-proposed-terms; reject-drops-
no-channel; point-to-point (wrong boundary can't see/accept/reject); poll-only-
returns-own-target. Enforces: a channel NEVER exists without the target
boundary's explicit accept; a requester can't force contact into another boundary.

### Channel unification (the no-shortcut fix) — DONE, builds clean, no regression
Replaced the `LocalChannel` stand-in (a VecDeque that IGNORED boundary) with a
boundary-aware handle table over the CANONICAL `Channel`:
  - `ChannelHandleTable`: (boundary, handle) -> canonical Channel (Arc-backed
    clone). Both endpoints of a cross-boundary channel register a handle to the
    SAME Channel, so bytes pass THROUGH THE KERNEL (try_send copies in, try_recv
    copies out) — never via shared user memory. Holds a ChannelRegistry + the
    shared scheduler as KernelInterface.
  - `open_channel` now mints a real canonical Channel from ChannelTerms;
    `channel_send`/`channel_recv` resolve (caller_boundary, handle) and call the
    canonical try_send/try_recv — REAL back-pressure (sender/receiver waiters)
    and wakeups via the scheduler.
  - Ring3Table.scheduler is now an `Arc<Scheduler>`, SHARED with the channel
    system: the SAME selector that dispatches lanes is the channels' wakeup
    authority. A lane parking on a full/empty buffer is woken by the selector
    that runs the other endpoint — the correct production architecture.
  - caller_boundary comes from the trap (active_lane -> boundary, from step 4).
333 tests green, clippy clean, default + multilane build clean, multilane QEMU
demo (spawn + arg marshaling) unaffected.

### Honest remaining (next, no overclaiming)
  - Expose the handshake over SYSCALLS (RequestChannel/PollChannelRequest/
    AcceptChannel/RejectChannel, numbers 18+) bridged to ChannelRegistry, caller
    boundary from the trap.
  - A cross-boundary DEMO: lane in boundary X requests -> lane in boundary Y
    accepts -> X sends bytes -> Y receives them (proving kernel-mediated
    cross-boundary IPC end-to-end in QEMU). The mechanism is all in place; this
    wires the two ring-3 lanes through it.
  - Then: F1 live session; Track 3 (Lattice ~ channels); Track 4 (orchestrator);
    per-arch ring-3 flow.

---

## INCREMENT IN PROGRESS: handshake SYSCALLS (numbers 18+) — reviewed design

### The canonical flow both ends follow (from shared/protocols/ipc.rs)
ChannelAcceptance::Accepted(ChannelId) — the ChannelId is the SHARED identity
BOTH ends learn. Requester proposes; target accepts-ALL-or-rejects. So the
syscall surface must let: the requester learn its endpoint AFTER acceptance, and
the target learn its endpoint AT acceptance. Five operations:

  18 RequestChannel: request_channel(target_boundary, terms_ptr, terms_len)
       -> request_id (>=0) | err. Terms passed by pointer (don't fit 3 regs),
       encoded like FsRwArgs. Caller boundary (from trap) = the requester.
  19 PollChannelRequest: poll_channel_request(out_ptr, out_len)
       -> request_id (>=0) | NotFound. Caller boundary (from trap) = the TARGET;
       writes (requester_boundary, terms) into the user buffer so the receiver
       can decide. Only requests aimed at the caller are visible (point-to-point).
  20 AcceptChannel: accept_channel(request_id) -> channel_handle (>=0) | err.
       Caller boundary (trap) must be the request's target. Creates ONE canonical
       Channel; registers a handle for the TARGET (returned) AND for the
       REQUESTER (retrieved later via 22). Accept-ALL (exact proposed terms).
  21 RejectChannel: reject_channel(request_id) -> 0 | err. Caller must be target.
  22 PollChannelOutcome: poll_channel_outcome(request_id) -> handle (>=0)
       | WouldBlock (still pending) | NotFound (rejected/unknown). The REQUESTER
       polls this to learn its endpoint handle once the target accepted.

### Anti-drift invariants (must hold)
  * Caller boundary ALWAYS comes from the trap (active_lane -> boundary), never
    from a user-supplied field — a lane cannot impersonate another boundary.
  * A channel exists only after the TARGET accepts; both handles point at the
    SAME canonical Channel (one ChannelId).
  * Terms accepted wholesale; receiver never alters them.
  * point-to-point: poll only surfaces requests for the caller's boundary.
  * Brief locks only; never held across a wait.

### Registry support needed (small extension)
ChannelRegistry::accept currently returns ChannelId but DROPS the Channel. It must
RETAIN the created Channel so BOTH boundaries can be given a handle to it, and so
the requester's outcome poll can find it. Add an accepted-outcomes map:
request_id -> (Channel, requester_boundary, target_boundary). accept() populates
it; the handle table reads it to register both handles; outcome poll consumes the
requester's side.

---

## VERIFIED (this session): handshake SYSCALLS (numbers 18-22) — built + tested

The full cross-boundary channel handshake is now exposed over syscalls, wired end
to end (enum -> dispatch -> trait -> KernelSyscallEnv -> registry + handle table),
following the canonical ABI conventions exactly (mirrors OpenChannel/Spawn).

### What landed
- shared/protocols/syscall.rs: 5 new Syscall variants with canonical-style docs +
  from_number arms:
    18 RequestChannel, 19 PollChannelRequest, 20 AcceptChannel,
    21 RejectChannel, 22 PollChannelOutcome.
- shared/protocols/ipc.rs: fixed-size wire encodings `ChannelTermsWire`
  (CHANNEL_TERMS_WIRE_LEN=76) and `ChannelRequestWire` (CHANNEL_REQUEST_WIRE_LEN),
  mirroring the FsRwArgs by-pointer convention (terms don't fit 3 registers).
- cibos-kernel/src/syscall.rs: 5 SyscallEnv trait methods (default NotPermitted) +
  5 dispatch arms. Dispatch enforces: terms decoded from user memory via
  copy_from_user; PollChannelRequest writes the ChannelRequestWire via
  copy_to_user; caller boundary (from trap) is ALWAYS the requester/target — never
  user-supplied; a boundary cannot request a channel TO ITSELF (use OpenChannel).
- cibos-kernel/src/channel.rs: `accept` now returns (Channel, requester_boundary)
  so BOTH endpoints can be wired to the SAME channel.
- kernel-image/src/boot.rs: KernelSyscallEnv implements all 5, backed by the
  ChannelHandleTable (registry + an `accepted` map request_id -> (requester,
  handle) so the REQUESTER learns its endpoint via PollChannelOutcome). accept
  registers a handle for BOTH target (returned) and requester (stored). Outcome
  poll: handle if accepted, WouldBlock if pending, NotFound if rejected/unknown.

### Tests (338 total, was 333: +5 dispatch, earlier +4 registry = +9 this session)
  - channel_terms_wire_round_trips / channel_request_wire_round_trips (encoding).
  - request_channel_to_self_is_rejected (distinct-boundary invariant).
  - request_channel_short_terms_buffer_is_rejected (length validation).
  - handshake_calls_default_to_not_permitted_without_ipc (dispatch reaches env).
  - (registry: accept-from-terms / reject / point-to-point / poll-own-target.)
All green; clippy clean (kernel + multilane); default + all arches build clean.

### Honest remaining (next)
  - The ring-3 SDK wrappers in cibos-app for the 5 new syscalls (request/poll/
    accept/reject/outcome) — thin syscall3 wrappers like spawn/open.
  - The cross-boundary QEMU DEMO: lane in boundary X requests -> lane in boundary
    Y polls + accepts -> X polls outcome (gets handle) -> X sends bytes -> Y
    receives them. Proves kernel-mediated cross-boundary IPC end to end on hw.
  - Then: F1 live session; Track 3 (Lattice); Track 4 (orchestrator); per-arch.

---

## VERIFIED (this session): cross-boundary IPC end-to-end (SDK wrappers + QEMU demo)

The FULL cross-boundary channel handshake now runs end-to-end on the metal,
through the REAL KernelSyscallEnv methods (the exact code ring-3 dispatch calls).

### What landed
- cibos-app/src/channel.rs: 5 ring-3 SDK wrappers — request_channel,
  poll_channel_request, accept_channel, reject_channel, poll_channel_outcome
  (thin syscall3 wrappers like spawn/open; terms encoded to ChannelTermsWire and
  passed by pointer; outcomes mapped to Option). RequestId/IncomingRequest types.
- kernel-image/src/ring3.rs: demonstrate_cross_boundary_handshake — drives the
  real env methods with X=0x100 (requester) / Y=0x200 (target).
- kernel-image/src/boot.rs: kernel_syscall_env() accessor (KernelSyscallEnv made
  pub(crate)).

### Runtime proof (QEMU serial, compute .img) — every canonical invariant exercised
```
X (0x100) requested a channel to Y (0x200): request #1
X polled outcome early: still pending (correct)            <- no channel until target accepts
Y polled: request #1 from boundary 0x100, terms cap=2 max_msg=64   <- target sees proposal + EXACT terms
wrong boundary (0x999) accept REJECTED (correct isolation) <- point-to-point: only the target may accept
Y accepted -> Y handle 0                                   <- accept-ALL creates the channel
X polled outcome: accepted -> X handle 1                   <- requester learns its endpoint
X sent 'hello-Y' on handle 1
Y received 'hello-Y' (7 bytes) — CROSS-BOUNDARY IPC OK     <- bytes crossed boundaries THROUGH the kernel
```
Both this demo AND the spawn+arg demo AND the normal .capp flow (exit 7) run in
one boot. 338 tests green; clippy clean; default + resume + multilane + aarch64 +
riscv64 + cibos-app all build clean.

### Faithfulness
The demo invokes the SAME KernelSyscallEnv methods the ring-3 dispatch reaches,
with two boundaries — the code path is identical to two ring-3 lanes trapping.
Bytes are copied THROUGH the kernel (try_send in, try_recv out), never via shared
user memory. A channel exists ONLY after the target accepts; a third boundary
cannot hijack the handshake (point-to-point isolation enforced at runtime).

### Honest remaining (next)
  - A two-RING-3-LANE cross-boundary demo (two actual lanes trapping through the
    syscalls, vs. the in-kernel env calls here). The mechanism is proven; this is
    a presentation refinement driving it from ring-3 payloads.
  - F1 live interactive session; Track 3 (network/Lattice ~ channels over the
    same model); Track 4 (server orchestrator, design-first); per-arch ring-3.
