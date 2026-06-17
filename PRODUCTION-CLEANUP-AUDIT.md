# Production Cleanup Audit — remove QEMU-orientation, ship bare-metal-first

GOAL: we are past the "prove it in QEMU" phase. The kernel must be PRODUCTION
bare-metal code. QEMU (or any hypervisor, or real hardware) merely VERIFIES it.
This audit classifies every demo-gate, selftest, and QEMU reference in the
shipping kernel as one of:
  [KEEP-OPTIN]  legitimate test scaffolding — opt-in feature, never in production,
                AND the capability it exercises ALSO exists in the production path.
  [PROMOTE]     production capability currently trapped in a demo/gate — must move
                into the always-on production boot path (like virtio-net just did).
  [RENAME]      production code mis-named/mis-described as a "demo" — rename to its
                production role; keep behavior.
  [DELETE]      dead QEMU-only scaffolding with no production purpose.

## A. Feature flags (kernel-image/Cargo.toml) — classification

| Feature | Class | Disposition |
|---|---|---|
| `self-boot` | KEEP-OPTIN | Real-hw default already reads the CIBIOS handoff; `self-boot` synthesizes one for `-kernel` boot. This is the CORRECT posture (bare-metal default, QEMU opt-in). Keep, but verify default truly needs no QEMU. |
| `storage-selftest` | KEEP-OPTIN | A non-destructive write/read round-trip — a genuine selftest. Production must NOT scribble each boot. Keep opt-in. BUT the *capability* (mount + read/write CIBOSFS) must be in production (it is: `mount_root_fs_early`). |
| `virtio-net-demo` | DONE→KEEP-OPTIN | Already fixed: driver is production + probed at boot; feature now only adds a verbose log line. Keep as logging-only. |
| `app-hello` | KEEP-OPTIN | `hello` is a pure smoke-test app. Opt-in is correct. |
| `app-login`/`app-shell` | PROMOTE(review) | The login + shell are the PRODUCTION interactive surface, not demos. They should be ON in a normal interactive image. Keep selectable (a headless server image may omit them) but ensure the production posture runs them via the LIVE path, not the injected one. |
| `interactive-session` | RENAME/PROMOTE | This is the REAL login→shell on the live keyboard — the production interactive boot. The injected `storage-selftest` login/shell block is the test double. Production should use THIS; the injected block is the selftest. |
| `gui-demo` | PROMOTE | The Surface→VGA display driver + GUI runner is PRODUCTION capability (the display path). It must not live only inside a demo. Promote the driver to always-compiled + a production GUI boot path; keep a thin demo only as an app choice. |
| `channel-demo` | KEEP-OPTIN | Exercises OpenChannel/Send/Recv via the ABI. The channel ABI itself is production (handle_syscall). Demo is a selftest — keep opt-in. |
| `ring3-resume-demo` | KEEP-OPTIN | Proves the park/resume mechanism. The mechanism is production (used by multilane + spawn). Demo is a selftest. |
| `ring3-multilane-demo` | PROMOTE(review) | Contains the cross-boundary channel handshake demo AND the Lattice demo AND installs the channel table. The channel TABLE install + the net/channel syscalls must be available in PRODUCTION, not only here. Promote the table install; keep the *demonstrations* opt-in. |
| `profile-*` | KEEP | Real ADR-007 profile bundles — production. |

## B. Mis-named production code (RENAME)

| Symbol (boot.rs) | Reality | Action |
|---|---|---|
| `run_ring3_demo` | The PRODUCTION ring-3 boot path: installs GDT/TSS/IDT, remaps PIC, inits PIT, enables IRQs, runs real `.capp` apps. Not a demo. | RENAME → `start_ring3_runtime` (or `bring_up_ring3`); the demos inside stay `#[cfg]`-gated. |
| `demonstrate_keyboard_input` | Production: enables interrupts + confirms IRQ1 input is live. | RENAME → `arm_keyboard_input` / fold into bring-up; keep the brief probe, drop "demonstrate". |
| `demonstrate_container_isolation` | Production-meaningful (proves per-boundary page isolation) but is a boot-time SELFTEST. | KEEP behavior; either gate behind a `selftest` feature or rename to `verify_container_isolation` and keep (cheap, valuable). Decide: keep on (cheap correctness check) but rename. |
| `demonstrate_storage` | Mixed: the mount is production; the round-trip is `storage-selftest`. | Split: production `mount_root_fs_early` stays; rename the rest `storage_selftest`, fully behind the feature. |

## C. QEMU-specific residue (DELETE / move to selftest only)

| Item | Action |
|---|---|
| `inject_text`/`inject_enter` + the injected login/shell block | These exist because "QEMU sendkey is unreliable". They are a TEST DOUBLE for the live keyboard. Move ENTIRELY behind `storage-selftest` (or a `boot-selftest` feature); the PRODUCTION interactive path is `interactive-session` (live keyboard). No injected input in production. |
| Comments referencing QEMU `sendkey`/monitor in the live path | Trim to hardware-first language; QEMU is only a verification note. |
| `synthesize handoff for standalone QEMU` (self-boot) | KEEP (that's what self-boot is for) but ensure it is ONLY under `self-boot`, never default. |

## D. The production posture we want (target state)
- DEFAULT build = bare-metal production: reads the real CIBIOS handoff, sets up
  hardware, probes real devices (ATA, NIC), runs the real interactive surface
  (live login→shell) and/or server services. NO injected input, NO demo prints,
  NO QEMU assumptions.
- `self-boot` = the ONLY QEMU/`-kernel` affordance (synthesize handoff).
- Each demo/selftest = a clearly-named opt-in feature that ADDS a verification
  routine; never the home of a production capability.
- Verification happens by BOOTING THE PRODUCTION IMAGE (under QEMU now, real
  hardware later) — not by building a separate "demo" image.

## E. Execution order (each step: build-all + tests + boot-verify)
1. RENAME `run_ring3_demo`→`start_ring3_runtime`, `demonstrate_keyboard_input`→
   `arm_keyboard_input`; update call sites + comments (no behavior change).
2. PROMOTE the channel-table install out of `ring3-multilane-demo` into the
   production bring-up (so channel + net syscalls work in a normal boot).
3. PROMOTE the GUI display driver (Surface→VGA) to always-compiled; add a
   production GUI boot path; keep `gui-demo` as just an app selection.
4. Move `inject_text`/`inject_enter` + injected login/shell FULLY behind
   `storage-selftest`; make `interactive-session` (live) the production surface.
5. RENAME `demonstrate_storage`→split mount(prod)/selftest(feature);
   `demonstrate_container_isolation`→`verify_container_isolation` (keep, cheap).
6. Audit comments: hardware-first wording; QEMU only as a verification aside.
7. Confirm DEFAULT image boots on bare-metal posture (real handoff path) with no
   demo output; verify via QEMU self-boot AND document the real-hw boot.

---

## PROGRESS (this session)

DONE:
1. [RENAME] `run_ring3_demo` → `start_ring3_runtime` (the production ring-3
   bring-up: GDT/TSS/IDT, PIC, PIT, IRQs, runs `.capp`s). Doc updated to
   production language. `demonstrate_keyboard_input` → `arm_keyboard_input`.
2. [PROMOTE] Channel + Lattice handle table is now installed in the PRODUCTION
   boot (`start_ring3_runtime`), backed by a production `Scheduler`, so the IPC
   syscalls (OpenChannel/Channel*/handshake) and net syscalls (GateBind/Connect/
   Link*/Warden*/Probe) work in a NORMAL boot — not only inside the multilane
   demo. Un-gated `install_channel_table`, `multilane_seed`, `ChannelHandleTable::
   new` from `ring3-multilane-demo` to production (`target_arch` only). The two
   genuinely demo-only helpers (`kernel_syscall_env`, `clear_channel_table`,
   used only by the in-kernel demos) stay gated to the demo.
3. [FIX] `gui-demo` no longer uses the injected-input test double; it drives the
   notepad GUI from the LIVE keyboard (production behavior), matching real hw.
4. VERIFIED: production image (NO demo features) boots, runs a `.capp` in ring 3,
   installs the channel/Lattice table, probes the NIC (real MAC) — clean. 353
   tests green; default + storage-selftest + interactive-session + multilane +
   gui-demo + channel-demo + aarch64 + riscv64 all build clean.

CONFIRMED-ALREADY-CORRECT:
- The DEFAULT build is bare-metal-first: `obtain_handoff` under `not(self-boot)`
  reads the REAL CIBIOS handoff via `core::ptr::read(ptr)`; only `self-boot`
  synthesizes one for `-kernel`. The production boot foundation was already right.

REMAINING (next):
5. [PROMOTE] GUI display driver (Surface→VGA, `crate::gui`): make it always-
   compiled production code with a production GUI boot path; `gui-demo` becomes
   just an app-selection, not the home of the driver.
6. [MOVE] `inject_text`/`inject_enter` + the injected login/shell block: keep
   FULLY behind `storage-selftest` as the deterministic SELFTEST double; the
   production interactive surface is `interactive-session` (live keyboard). (They
   are already storage-selftest-gated; verify nothing else depends on them and
   that the production posture uses the live path.)
7. [RENAME] `demonstrate_storage` → split prod mount vs `storage_selftest`;
   `demonstrate_container_isolation` → `verify_container_isolation` (keep, cheap).
8. [AUDIT] remaining QEMU-wording in comments → hardware-first; QEMU only as a
   verification aside.
9. Confirm the production interactive image runs login→shell on the LIVE keyboard
   as the default posture (interactive-session), with the injected path used only
   for CI-style selftest.
