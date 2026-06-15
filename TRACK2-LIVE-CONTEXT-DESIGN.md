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
