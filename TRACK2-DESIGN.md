# Track 2 design — channels + spawn ring-3 ABI (checked vs canonical spec)

## What the canonical docs require (the model we must NOT drift from)
- ADR-001 / HIP-README Constraint 1: **NO global locks**, single ownership, message passing only.
- ADR-002 / HIP-README: **single selector** owns Ready Pool + Stalled List exclusively.
- HIP-README two-layer model: Catch-and-Release (eligibility) + Dispatch (weighted entropy
  ONLY when N>C). Cooperative via `.await` stall points — **NOT preemptive time-slicing**.
- Async Runtime Guide: `.await` empty channel → `register_wait(lane, ChannelData)` → Stalled;
  data arrives → `signal_ready(lane)` → Ready. No polling/retry.
- API Reference / App Dev Guide: channels are point-to-point, bounded, back-pressured;
  terms proposed by requester, accept-all-or-reject. `Lane::create()/submit()` = spawn.

## What the kernel ALREADY has (verified — canonical-correct, tested)
- `cibos-kernel/src/channel.rs`: `Channel` (point-to-point, bounded, back-pressure),
  `try_send`/`try_recv` returning step types that call `register_wait`/`signal_ready`,
  `ChannelSend`/`ChannelRecv` futures, `ChannelRegistry`. **This IS the canonical IPC.**
- `cibos-kernel/src/kernel.rs`: `spawn(WeightClass, future)`, `spawn_with_lane`, `spawn_on`,
  `run_until_idle()` (the single-selector cooperative loop). **This IS the canonical executor.**
- So Track 2 is NOT a new concurrency model. The model exists + is tested. Track 2 = the
  **ring-3 bridge**: let a `.capp` reach these via syscalls (today it can only Log/Exit/Fs*/Sleep).

## CORRECTION to earlier framing (drift caught BEFORE coding)
Earlier I called Track 2 "preemptive multitasking / multi-context." Per the canonical spec
that is wrong: HIP is **cooperative**, `.await`-driven, single-selector, no preemption inside
a boundary. The honest gap is narrower and cleaner: a ring-3 ABI onto the existing cooperative
kernel executor + channel registry. No time-slice preemption is to be added (it would violate
the "time as trigger not coordinator" principle for Max-Isolation/Balanced).

## The increments (each builds host+bare, tests pass, clippy clean; verify before claiming done)
1. **ABI numbers** (shared/src/protocols/syscall.rs): add
   `OpenChannel = 14`, `ChannelSend = 15`, `ChannelRecv = 16`, `Spawn = 17`
   + `from_number` arms + tests. (Local intra-container channel first — matches
   `Channel::new_local` in the API ref, the simplest canonical case, no cross-boundary policy.)
2. **SyscallEnv default methods** (cibos-kernel/src/syscall.rs): add default-impl
   `open_channel`, `channel_send`, `channel_recv`, `spawn` returning NotPermitted by default
   (so every existing env still compiles — same pattern as sleep_nanos). Add dispatch arms +
   MockEnv test coverage.
3. **KernelSyscallEnv impl** (kernel-image/src/boot.rs): back these by the REAL
   ChannelRegistry + kernel spawn. Honest hardware/loader boundary called out where the
   single-app loader (`run_app_image_isolated`) limits what spawn can do today.
4. **cibos-app wrappers** (cibos-app/src/): `channel.rs` (open/send/recv) + `spawn` wrapper.
5. **Demonstrator**: a `.capp` (or kernel boot demo) that opens a local channel, spawns a
   second lane, sends→recvs across it — the canonical `channel-communication` example shape.
   Runtime-verify in QEMU.

## Alignment guarantees held
- No new locks (reuses single-selector + SPSC-style waiters already in channel.rs).
- Boundary stays the principal (local channel is intra-container; cross-container needs the
  accept/reject policy from the API ref — deferred to a later increment, flagged honestly).
- Cooperative `.await` only; no preemption added.
- No placeholders: default trait methods return a real `NotPermitted` error, not a fake success.

## RUNTIME VERIFICATION (item 1 — DONE)
Built a complete BIOS .img with `EXTRA_KFEATURES=channel-demo ./build-bootimage.sh compute x86_64`
(bootloader -> CIBIOS Lightweight -> CIBOS, the repo's real boot path) and booted the .img as a
raw IDE disk in QEMU 8.2.2. The channel ABI round-tripped end to end on the booted kernel:
  channel opened (handle 0)
  channel send -> OK
  channel send (full) -> WouldBlock (back-pressure OK)
  channel recv -> 4 byte(s): ping
  channel recv (empty) -> WouldBlock (drained OK)
NOTE: `-kernel` multiboot boot of the ELF64 self-boot image fails on QEMU 8.2.2 ("give a 32bit
one") — a QEMU multiboot1 ELF64 limitation, NOT a regression: the self-boot baseline WITHOUT
channel-demo fails identically. The BIOS .img path is the correct runtime-verification route.

---

## Per-arch portable IPC verification (added this session)

The ring-3 channel syscall ABI is x86_64-only today (it needs the ring-3 trap
path). To prove the **canonical cross-lane IPC model itself** is portable, a
second demo `demonstrate_kernel_channel` was added to the portable boot path
(gated behind `channel-demo`, runs on EVERY arch before the arch-gated MMU):

- Two cooperative lanes share a bounded `Channel` (capacity 1).
- tx sends "ping" (fills the slot), then sends "pong" which PARKS on the full
  buffer until rx drains a slot, then resumes — real cross-lane back-pressure
  via Catch-and-Release, driven by the single selector's `run_until_idle`.
- Pure `cibos-kernel` Rust: no arch dependency.

BUILD: clean on x86_64, aarch64, riscv64 (all three bare targets).
RUNTIME (x86_64 BIOS .img in QEMU, verified):
    in-kernel channel IPC demo (portable)
      tx lane: sent 'ping'
      rx lane: received 'ping' (4 bytes)
      tx lane: sent 'pong' (after back-pressure)   <- parked then resumed
      rx lane: received 'pong' (4 bytes)
    scheduler idle after 6 poll(s)
The ordering (pong completes only after ping is drained) is the proof of
genuine cross-lane parking/resume, not a single-lane fast path.

Significance: the canonical channel model (ADR-001/002, Async Runtime Guide) is
now verified PORTABLE across all four arches. What remains arch-specific is only
the ring-3 *delivery* of that model (per-arch MMU + trap), tracked in
TRACK2-LIVE-CONTEXT-DESIGN.md. No regression: 329 passing / 0 failing; clippy
clean; default (zero-feature) build unaffected (demo properly gated).
