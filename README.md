# CIBOS / CIBIOS / HIP

A privacy-focused operating system built from scratch in Rust as a single Cargo
workspace. **CIBIOS** is the firmware (replaces BIOS/UEFI), **CIBOS** is the
microkernel OS, and **HIP** (Hybrid Isolation Paradigm) is its isolation model:
the security principal is the *boundary*, not a user account, and isolation is
binary — maximal or none, never tiered.

Everything here is real, compiles, and is covered by **298 unit tests** plus
doctests and the application/example suites. There are no placeholders or mocks.
Where a capability genuinely depends on hardware, that boundary is called out
honestly in the docs rather than faked.

The from-scratch boot path and the on-kernel runtime are **runtime-verified in
QEMU** (used as a hardware stand-in, not a shortcut): BIOS → custom MBR → CIBIOS
firmware → CIBOS kernel → weighted-entropy scheduler → MMU with per-container
address spaces → ring-3 user mode → `int 0x80` syscalls → PS/2 keyboard IRQ →
PIT timer → ATA-PIO block storage → an on-disk filesystem (**CIBOSFS**) → and
real `.capp` applications (including a **Rust** program built on the `cibos-app`
runtime) loaded into ring 3 that reach the kernel through the syscall ABI.

## What's built

* **Firmware (CIBIOS)** booting **four architectures** — x86-64, AArch64,
  RISC-V 64, and **32-bit x86 for old hardware** — with entropy gathering,
  image verification, and handoff. See `ARCHITECTURES.md`.
* **Microkernel (CIBOS)** — a weighted-entropy HIP scheduler, a custom
  Catch-and-Release async runtime (not tokio), isolation boundaries, bounded
  channels with back-pressure, and a memory manager.
* **A verified firmware→kernel boot chain** and a host-side **SPHINCS+ image
  signing pipeline** (`tools/mkimage`). See `SECURITY-NOTES.md`.
* **SDK** — channels, task spawning, timers, a shared **filesystem**, and the
  **Lattice** network fabric.
* **Four platforms** — CLI, GUI (cell-grid display), mobile (touch gestures),
  and server (headless daemon). See `PLATFORMS.md`.
* **Networking** — the Lattice (stack), Gates (ports), Links (connections),
  Warden (firewall), Probe (scanner), a live **Vane** server daemon, the
  **Lens** browser, and the **Hail** request protocol. See `NETWORKING.md`.
* **Security** — boundary isolation, password / wired-key-device authentication
  (`accounts`), a CLI **login** gate, and a mobile **PIN lock screen**.
* **Bare-metal on-kernel runtime (runtime-verified in QEMU)** — the kernel
  builds its own page tables and switches `CR3`; per-container address spaces
  (a page mapped in one boundary is physically absent in another); ring-3 user
  mode with an `int 0x80` syscall transport (`Log`, `Exit`, `FsRead`/`FsWrite`/
  `FsMkdir`/`FsExists`); a remapped-PIC PS/2 keyboard IRQ path with a portable
  scancode decoder; a PIT system timer (the wake/timeout source); an **ATA-PIO**
  block driver; **CIBOSFS**, an on-disk filesystem (superblock + block bitmap +
  inodes + directories) over a portable `BlockDevice` trait; and a `.capp`
  external-application format whose images are loaded into ring 3 — including a
  **Rust** application built on the `cibos-app` `no_std` runtime.
* **`cibos-app`** — the `no_std` runtime a ring-3 `.capp` links against: console
  output, filesystem access, and exit, all over the syscall ABI.
* **Storage** — **Live** (RAM-only, wiped on shutdown, no trace) and
  **Persistent** (partition-backed) volumes. *(Host model today; CIBOSFS is the
  on-disk backing the Persistent volume is being wired onto.)*
* **Applications** — package manager, app store (Trove), shell, text editor,
  key-value store, calculator IPC service, port scanner, web server + browser,
  notepad (GUI), messaging (Courier), email (Postbox), contacts, calendar.
  *(All 17 build and pass tests as host programs run in-process via `AppHost`;
  porting them onto `cibos-app` to run as on-kernel `.capp`s is in progress.)*

## Workspace layout

```
shared/                foundation: types, crypto (SHA-256, SPHINCS+/ML-KEM/ML-DSA), protocols
cibios/                firmware: boot, detection, image verify, handoff (4 arches)
cibos-async-runtime/   Catch-and-Release executor
cibos-kernel/          HIP scheduler, channels, isolation, memory, block + CIBOSFS, syscalls
cibos-sdk/             app SDK: System, channels, fs, Lattice
cibos-app/             no_std on-kernel app runtime (syscall/console/fs) a .capp links against
kernel-image/          bootable kernel binary; apps/ holds the .capp sample programs
tools/mkimage/         image build + SPHINCS+ signing/verification
platform-cli/ -gui/ -input/ -mobile/ -server/   the four platforms + input model
accounts/ login/       authentication and login gate
storage/               Live / Persistent volumes
applications/          package-manager, trove, shell, editor, kvstore,
                       calc-service, probe, vane, lens, web-protocol, notepad,
                       lockscreen, courier, postbox, contacts, calendar
```

## Building and testing locally

Prerequisites: a Rust toolchain via `rustup`. The whole workspace builds on
**stable**; only the 32-bit-x86 firmware needs **nightly** (for `build-std`).

```sh
# 1. Run the full test suite (stable).
cargo test --workspace \
  --features cibios/test-crypto,cibos-async-runtime/std,cibos-kernel/std,shared/pqc-full

# 2. Lint.
cargo clippy --workspace

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

## License

MIT — Copyright (c) 2026 RebornBeat (Christian). See `LICENSE`.
