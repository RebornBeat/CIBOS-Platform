# CIBOS-Platform

The unified home of three historically distinct, sequentially developed projects:
**HIP → CIBIOS → CIBOS**. A privacy-focused operating system built from scratch
in Rust.

* **HIP** (Hybrid Isolation Paradigm) — the isolation *model*: the security
  principal is the *boundary*, not a user account, and isolation is binary —
  maximal or none, never tiered.
* **CIBIOS** (Complete Isolation Basic Input/Output System) — the *firmware*
  that replaces BIOS/UEFI: entropy gathering, image verification, handoff.
* **CIBOS** (Complete Isolation-Based Operating System) — the *microkernel OS*
  built on HIP, booted by CIBIOS.

This repository consolidates all three into one Cargo workspace while preserving
each project's original commit history. The three original repositories remain
archived as independent historical records (see **Provenance** below).

Everything here is real, compiles, and is covered by an extensive automated test
suite plus doctests. There are no placeholders or mocks. Where a capability
genuinely depends on hardware (real NIC packets, a physical display, usermode
privilege separation, in-firmware PQC), that boundary is called out honestly in
the docs rather than faked.

## What's built

* **Firmware (CIBIOS)** booting **four architectures** — x86-64, AArch64,
  RISC-V 64, and **32-bit x86 for old hardware** — with entropy gathering,
  image verification, and handoff. See `ARCHITECTURES.md`.
* **Microkernel (CIBOS)** — a weighted-entropy HIP scheduler, a custom
  Catch-and-Release async runtime (not tokio), isolation boundaries, bounded
  channels with back-pressure, and a memory manager.
* **A verified firmware→kernel boot chain** and a host-side **SPHINCS+ image
  signing pipeline** (`tools/mkimage`). See `SECURITY-NOTES.md`.
* **Application SDK** — lanes (structured task spawning) with an `#[cibos::main]`
  entry macro and a `select!` macro; typed local channels with back-pressure;
  cross-container channels (request/accept routing over a multi-container host);
  timers with a host-driven monotonic clock; container introspection (id,
  resource limits, live channel counts, opt-in memory accounting); a shared
  **filesystem**; and the **Lattice** network fabric. Feature-gated profile
  extensions (per-lane scheduling weights) adapt one source to the compiled
  profile.
* **Four platforms** — CLI, GUI (cell-grid display), mobile (touch gestures),
  and server (headless daemon). See `PLATFORMS.md`.
* **Networking** — the Lattice (stack), Gates (ports), Links (connections),
  Warden (firewall), Probe (scanner), a live **Vane** server daemon, the
  **Lens** browser, and the **Hail** request protocol. See `NETWORKING.md`.
* **Security** — boundary isolation, password / wired-key-device authentication
  (`accounts`), a CLI **login** gate, and a mobile **PIN lock screen**.
* **Storage** — **Live** (RAM-only, wiped on shutdown, no trace) and
  **Persistent** (partition-backed) volumes.
* **Applications** — package manager, app store (Trove), shell, text editor,
  key-value store, calculator IPC service, port scanner, web server + browser,
  notepad (GUI), messaging (Courier), email (Postbox), contacts, calendar,
  clock.

## Workspace layout

```
shared/                foundation: types, crypto (SHA-256, SPHINCS+/ML-KEM/ML-DSA), protocols
cibios/                firmware: boot, detection, image verify, handoff (4 arches)
cibos-async-runtime/   Catch-and-Release executor
cibos-kernel/          HIP scheduler, channels, isolation, memory
cibos-macros/          #[cibos::main] and select! procedural macros
cibos-sdk/             app SDK: System, lanes, channels (local + cross-container),
                       timers, fs, Lattice, container introspection, multi-container host
cibos-input/           shared input model
kernel-image/          bootable kernel binary
tools/mkimage/         image build + SPHINCS+ signing/verification
platform-cli/ -gui/ -mobile/ -server/   the four platforms
accounts/ login/       authentication and login gate
storage/               Live / Persistent volumes
applications/          package-manager, trove, shell, editor, kvstore,
                       calc-service, probe, vane, lens, web-protocol, notepad,
                       lockscreen, courier, postbox, contacts, calendar, clock
targets/               custom target spec (i686-cibos-none)
```

## Building and testing locally

Prerequisites: a Rust toolchain via `rustup`. The whole workspace builds on
**stable**; only the 32-bit-x86 firmware needs **nightly** (for `build-std`).

```sh
# 1. Run the full test suite (stable). Print the count to confirm it for yourself.
cargo test --workspace \
  --features cibios/test-crypto,cibos-async-runtime/std,cibos-kernel/std,shared/pqc-full

# Exercise the SDK's optional features too (per-lane weights, host memory tracking):
cargo test -p cibos-sdk --features "dynamic-weights host-memory-tracking"

# 2. Lint (treat warnings as errors).
cargo clippy --workspace --all-targets -- -D warnings

# 3. Build the firmware for the three stable bare targets.
rustup target add x86_64-unknown-none aarch64-unknown-none riscv64gc-unknown-none-elf
cargo build -p cibios --target x86_64-unknown-none
cargo build -p cibios --target aarch64-unknown-none
cargo build -p cibios --target riscv64gc-unknown-none-elf

# 4. Build the firmware for 32-bit x86 (old hardware) — needs nightly.
rustup toolchain install nightly --profile minimal
rustup component add rust-src --toolchain nightly
./build-i686.sh        # -> target/i686-cibos-none/debug/cibios (ELF32 i386)

# 5. Build and sign a bootable image (host pipeline).
cargo run -p mkimage -- keygen keys/root.pub keys/root.key
# (see BOOT.md for flattening a kernel ELF and producing/verifying a .cimg)
```

> Note: the test count varies with which feature combinations you enable. Run
> the commands above and read the totals off your own machine rather than
> trusting a number quoted here.

### Runnable demos (on the host)

```sh
# Interactive system shell (package manager + kv store + editor + fs commands)
cargo run -p shell --bin cibos-shell
#   try: help, kv set name CIBOS, kv get name, edit append hi, pkg list, write /f hi, read /f

# Web stack: a Vane server and a Lens client over the Lattice
cargo run -p lens --bin web-demo

# GUI notepad rendered to the virtual display after scripted input
cargo run -p notepad --bin gui-demo
```

### SDK examples

```sh
cargo run -p cibos-sdk --example hello_main             # minimal #[cibos::main] app
cargo run -p cibos-sdk --example hello_lane             # lane + timer + join
cargo run -p cibos-sdk --example parallel_computation   # parallel lanes
cargo run -p cibos-sdk --example pipeline_processing    # 3-stage channel pipeline
cargo run -p cibos-sdk --example profile_flexible       # adapts to per-lane-weights feature
cargo run -p cibos-sdk --example channel_communication  # cross-container channels
```

### QEMU / hardware

Booting the firmware + kernel image under QEMU for each architecture is
described in `BOOT.md` (and `kernel-image/QEMU.md`). Running on real hardware,
real NIC-backed networking, and in-firmware PQC verification are the
hardware/validation-gated items noted in `SECURITY-NOTES.md` and `NETWORKING.md`.

## Design docs

* `ARCHITECTURES.md` — the four CPU targets and the 32-bit-x86 rationale.
* `PLATFORMS.md` — CLI / GUI / mobile / server platforms and the input model.
* `NETWORKING.md` — the Lattice vocabulary and the loopback-vs-NIC boundary.
* `SECURITY-NOTES.md` — image signing, and why in-firmware PQC needs a `no_std`
  verifier.
* `BOOT.md` — the full firmware→kernel QEMU boot guide.

## Provenance

This repository was assembled by merging three originally separate repositories,
preserving each one's full commit history and ancestry:

1. **HIP** — Hybrid-Isolation-Paradigm-HIP
2. **CIBIOS** — CIBIOS-Complete-Isolation-Basic-Input-Output-System
3. **CIBOS** — CIBOS-Complete-Isolation-Based-Operating-System

The three original repositories are preserved as **archived, read-only**
historical records. Their original creation dates and commit timestamps remain
the authoritative record of when each project began. To inspect the unified
ancestry:

```sh
git log --graph --oneline --all
```

## License

MIT License — Copyright (c) 2026 RebornBeat
See LICENSE for the full text.
