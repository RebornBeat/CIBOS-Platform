# App-Layer Port — Review Findings & Corrected Plan

This document is the authoritative record of the full review requested after the
`login` `.capp` create-user milestone, capturing goals, confirmed drift, and the
ordered, non-drifting path forward. Written from a full read of the actual files
(not from test results).

## The enduring goal (unchanged)

Bare-metal CIBOS that boots power-on → our bootloader → kernel, and delivers the
real interactive product: **create-user → login → shell → install app from a
local repo → run it**, with the **17 existing applications** as the app layer,
real-hardware-first (QEMU only as the test harness).

## Verified-done lower stack (runtime-proven in QEMU)

boot chain → MMU → per-container isolation (mechanism) → ring-3 → syscall
transport → PS/2 keyboard (IRQ) → PIT timer → ATA block driver → CIBOSFS →
filesystem syscalls → `get_random` (kernel CSPRNG) → `cibos-app` runtime
(console, fs, heap/alloc, input incl. `read_line`+masking, rand, time) → a Rust
`.capp` doing heap alloc + fs round-trip in ring 3 → a `login` `.capp` that
completed **create-user** (masked password, CSPRNG salt, credential persisted).

## Where we left off — the immediate decision

The `login` `.capp` create-user succeeded; the **second run failed** with
`map app segment page failed`. Root cause: every app runs in the **single shared
kernel address space** at fixed vaddrs (`run_app_image`, loader.rs:268 →
`crate::arch::paging::install(space.root())`), so re-running or running another
app re-maps already-mapped pages, which `AddressSpace::map` rejects.

DECISION (checked against the goal): build **per-process address spaces** as a
real kernel capability — each process gets its own page tables with the kernel
mapped in, runs, is torn down; `exit` returns to a launcher. NOT a
skip-if-mapped patch (fragile), NOT a one-off helper bolted onto the boot demo.
This is the process model the OS needs for the login→shell hand-off anyway.

## Confirmed drift / shortcuts (to correct)

1. **`login-rs` reimplements login + invented a second credential format.**
   - `kernel-image/apps/login-rs` writes a `shared::crypto::credential::CredentialRecord`
     under `/etc/passwd.d/` — a NEW on-disk format.
   - The real `login::run_login` + `accounts` crate already implement login
     (salted SHA-256 `Verifier::Password`, attempt countdown, key-device
     handling, tested), used by nothing on the kernel.
   - Result: TWO login implementations + TWO credential formats that don't
     interoperate. `hello-rs` is fine (pure bring-up vehicle); `login-rs`
     overstepped into reimplementing a real subsystem. CORRECT: drive the real
     `login`/`accounts` on the kernel; one login, one format.

2. **The architecture was always meant to PORT, not rewrite.** `platform-cli`'s
   own doc: "A CIBOS display/TTY-backed console is a later addition; applications
   written against the `Console` trait do not change." The whole app layer rides
   on two seams: `Console` (8 apps + login) and SDK `System` (~14 apps +
   storage). Shell's actual usage is small and already has syscalls:
   `write_line`/`read_line`, `filesystem().{write,read,list,delete}`, `now()`,
   `resource_limits()`, plus `spawn` (synchronous for a CLI loop — no async
   kernel needed to run a shell).

3. **17 apps + 6 examples were only `cargo test`/`build`-ed, not read.** Now
   reviewed: 4 apps have `[[bin]]` (lens, notepad, shell), rest are libs composed
   by shell/trove/lens. The 6 SDK examples are async-runtime teaching demos
   (lanes/channels/pipelines) — host-only, not part of the on-kernel product.

4. **Other:** `wait_ticks_or` mis-grep → hand-rolled loop (already reverted).
   Storage Live/Persistent exist but in the std host model, NOT wired to CIBOSFS.
   SDK `spawn`/async on the real kernel is genuinely large — not a trivial
   recompile; treat honestly.

## `no_std` portability facts (measured, not assumed)

- `accounts`: only ONE `std::` use (`std::collections::BTreeMap` →
  `alloc::collections::BTreeMap`); already `#![forbid(unsafe_code)]`. Password
  path is pure SHA-256 (no_std). Knot: it pulls `shared` with `pqc-sphincs` for
  the key-device path — gate that so the password-only kernel build is clean.
- `login`: logic is over `Console` + `Accounts`; no inherent std beyond those.
- SDK (`cibos-sdk`, 3238 lines): the std boundary; embeds an in-process kernel
  (`AppHost`). Porting `spawn`/lanes/channels onto the real kernel scheduler is
  the large piece — defer; CLI apps run synchronously without it.

## Corrected plan (ordered, no drift)

1. **Per-process address spaces** — DONE, runtime-verified. `run_app_image_isolated`
   (loader.rs) builds a fresh `AddressSpace` per call, identity-maps the shared
   kernel range (`KERNEL_IDENTITY_MAP_BYTES`, single source of truth used by both
   `bring_up_mmu` and the launcher), maps the app's segments/stack/heap, installs
   the space, enters ring 3, and restores the caller's CR3 on `exit`. Each process
   is isolated and re-runnable. Proof: in one boot, `hello` (exit 7) → `hello-rs`
   (exit 9) → `login(create)` "alice created" (exit 0) → **`login(auth)` "welcome,
   alice" (exit 0)** — the second login run, which previously failed with
   `map app segment page failed`, now succeeds. Default + selftest both boot to
   `boot complete`; workspace 309/0; clippy clean.
   - KEPT (still valuable): `load_app_image`, `map_user_stack`,
     `enter_user_context`/`return_to_kernel`, `run_user_payload` (the documented
     diverging init-process entry), `demonstrate_container_isolation` (mechanism
     proof), the `.capp` embeds + keyboard/fs/inject demos.
   - RETIRED: the shared-space `run_app_image` (superseded; it caused the
     collision). All call sites moved to `run_app_image_isolated`.
2. **Retire `login-rs`'s reimplementation.** IN PROGRESS — foundations done:
   - DONE: collapsed the duplicate password hash — `accounts::password_hash` now
     delegates to `shared::crypto::credential::hash_password` (one construction).
   - DONE: persistence bridge — `accounts` gained `enroll_password_record` /
     `password_record_for` (`CredentialRecord` ↔ registry), so the on-disk format
     is owned through `accounts`. Cross-compat test proves a record and the
     registry agree byte-for-byte. accounts: 8 tests.
   - DONE: extracted the `Console` trait into a tiny `no_std` crate
     `cibos-console`; `platform-cli` re-exports it (its 8 app dependents + the
     `StdConsole`/`CaptureConsole` impls unchanged).
   - DONE: `accounts` and `login` are now `no_std + alloc` with a default `std`
     feature (host unchanged) and a `portable-pqc` feature (bare key-device path
     via the portable SPHINCS+ verifier). Both build host (std) and bare
     (x86_64-unknown-none). Fixed two real dependency bugs: cfg-on-own-features
     (key-device verifier selection) and the workspace-dependency
     `default-features = false` rule (was silently pulling std `pqc-sphincs` →
     `getrandom`, breaking the bare build).
   - DONE: `SyscallConsole` in `cibos-app` (impl `cibos_console::Console`:
     write_line→Log, read_line→ReadKey, read_secret→masked ReadKey). Added
     `read_secret` to the `Console` trait as an ADDITIVE default (falls back to
     read_line) so the 8 apps + host backends are unchanged; `login` opts in for
     the password only.
   - DONE: refactored `login` into `run_login` (prompts name) + `run_login_for`
     (gate for a known boundary) — one implementation, faithful (all 4 login
     tests pass), gives the kernel app a no-double-prompt entry.
   - DONE: rewrote the `login-rs` `.capp` to DRIVE the real `login::run_login_for`
     + `accounts` (loads/saves `/etc/passwd.d/<name>` via the `accounts`
     `CredentialRecord` bridge). No longer a reimplementation — a thin launcher;
     the FS-persistence glue is legitimately app-specific (login/accounts are
     storage-agnostic by design).
   - RUNTIME-VERIFIED: selftest boot runs the REAL gate end to end — create-user
     "alice created" (exit 0), then `login::run_login_for` → "welcome, alice"
     (exit 0). Default + selftest both reach `boot complete`. Workspace 311/0,
     clippy clean, accounts/login/cibos-console/cibos-app build bare.

   STEP 2 COMPLETE. (history) Once that runs on the kernel, RETIRE the parallel
   reimplementation and remove
     its now-redundant direct `CredentialRecord` usage. Keep `login-rs` running
     until then.
   - Workspace: 311/0; bare builds green; clippy pending re-check.
3. **Syscall-backed `Console` + `Filesystem`/`System` shim** so existing apps
   compile/run unchanged on the kernel (the seam the architecture intended).

   DESIGN (file-grounded, from reading shell's `dispatch` + SDK `System`/`Filesystem`):
   - The shell command logic (`dispatch`, private to shell) is fully SYNCHRONOUS
     and needs only: `console.{write_line}` (have: `SyscallConsole`),
     `system.filesystem().{write,read,list,delete}`, `system.now().as_nanos()`,
     `system.resource_limits().{memory_bytes,max_lanes,max_channels,max_message_bytes}`.
     NO spawn/channels/lattice in the actual logic — `spawn` is only the host
     AppHost wrapper, which the `.capp` process model replaces entirely.
   - SEAM: define minimal `no_std` traits for this surface (e.g. `FsApi` with
     write/read/list/delete; a `SystemApi` with `filesystem()/now()/resource_limits()`),
     make shell's `dispatch` generic over them (dispatch is private → safe).
     Provide TWO impls: the SDK `System`/`Filesystem` (host, behavior unchanged →
     existing shell tests stay green) and a `cibos-app` syscall-backed impl.
   - GAP to fill first: `cibos-app::fs` has read_into/write/mkdir/exists but NO
     `list`/`delete`, and there is no fs-list syscall yet. Add `FsList` + `FsDelete`
     syscalls (mirror the existing `Fs*` ABI) + kernel dispatch + `cibos-app` wrappers.
   - Then reuse `dispatch` VERBATIM on the kernel. No reimplementation of shell.

   FORMER step-3 text retained:
4. **Port shell** (the integrator) — synchronous command loop — then the libs it
   composes (package-manager, kvstore, editor).
5. **Wire storage Live/Persistent onto CIBOSFS**; **local package repo** on the
   medium; `--with-apps` flavor flag for `mkbootimage`.

## Guardrails (from lessons this project)

- `cat` whole files before concluding something is absent; never trust a narrow
  grep. Prefer existing tested primitives over parallel implementations.
- Read apps/examples, don't just run their tests.
- Every increment: build host + bare, run regression, clippy, and a QEMU runtime
  check before claiming "works" or refreshing the archive.
