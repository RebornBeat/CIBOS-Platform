# CIBOS / CIBIOS / HIP — Progress & Complete Roadmap

This document captures (1) what was accomplished across this chat session and
(2) **everything that remains to complete the whole system**, drawn from the
finalized HIP/CIBIOS/CIBOS docs and the alignment review. It is meant to be
exhaustive — nothing knowingly omitted.

---

## PART 1 — PROGRESS THIS SESSION

The session goal was to take the project from "boots only under QEMU's multiboot
loader (GRUB-style)" to a **from-scratch, self-contained bootable system** that
starts from a BIOS power-on with no external loader, and to prove it runs. That
was achieved and verified at runtime.

### 1.1 Established and protected the baseline
* Confirmed the real workspace on disk (30+ crates), the stable-1.96 toolchain,
  and the three bare targets.
* Verified the starting point by *running* it: host test suite **218 → now 225
  passed / 0 failed**; `cibios` and `kernel-image` build bare on x86_64.
* Recovered cleanly from a transient full-disk during cross-compiles (`cargo
  clean`; `target/` is regenerable).

### 1.2 Layer 1 — the bootloader→CIBIOS contract (`shared/src/protocols/boot.rs`)
* New `BootHandoff`, `BootLayoutDescriptor`, `BootMemoryRegion`, `BootRegionType`.
* `#[repr(C, align(8))]` so the on-disk/in-memory layout is **byte-identical on
  i686 and x86_64** (caught a real bug: `u64` aligns to 4 on i686, which would
  have silently corrupted the contract — fixed by forcing 8-byte alignment).
* Compile-time `offset_of!` assertions pin the ABI the assembly and tools bind to.
* `to_bytes`/`from_bytes` so the disk descriptor is serialized from the
  authoritative type (cannot drift). Honored `shared`'s `#![forbid(unsafe_code)]`
  by keeping the type pure-data and doing pointer reads in the consumer.

### 1.3 Layer 2 — CIBIOS entry split (multiboot vs from-scratch bootloader)
* Added `firmware-multiboot` (default, QEMU `-kernel`) vs `firmware-bootloader`
  (bare-metal, reads the `BootHandoff`) features, with `build.rs` guard rails
  (mutual exclusion; "x86 needs exactly one boot entry"). Mirrors the proven
  `kernel-image` `self-boot` split.
* New 64-bit and 32-bit boot entries (`boot/x86_64_bootloader.s`,
  `x86_bootloader.s`) that capture the handoff pointer (RDI / EAX).
* `arch/x86_64.rs` and `arch/x86.rs` gained cfg-split `locate_image`,
  `read_memory_map`, and a `boot_handoff()` reader (multiboot vs handoff source);
  `detect`/`putc`/`halt`/`jump_to_kernel` stayed shared. `run()`/`boot_image()`
  untouched. ARM/RISC-V unaffected (they boot via device tree).

### 1.4 Layer 3 — the from-scratch BIOS bootloader (`bootloader/`)
* `boot/stage1.S` — 512-byte MBR: EDD check, reads the layout descriptor, loads
  Stage 2, jumps.
* `boot/stage2.S` — real-mode A20 + E820 + chunked disk load (unreal-mode copies
  to high memory), builds the `BootHandoff`, sets up identity-mapped page tables,
  and transitions to long mode (x86_64) or protected mode (i686), jumping to the
  CIBIOS entry with the handoff pointer in the right register.
* `link/stage1.ld`, `link/stage2.ld`, `build.sh`, `README.md`. Toolchain is GNU
  binutils (`gcc -m32`/`ld`/`objcopy`) — the prior plan's `clang` dependency was
  dropped after checking what's actually installed. Mode-transition encodings
  verified at the byte level.

### 1.5 Layer 4 — image assembler (`tools/mkbootimage/`)
* New host crate: reads stage1/stage2/CIBIOS/CIBOS, computes sector-aligned LBAs,
  writes the bootable `.img`, serializing the descriptor from the `shared` type.
  Validates Stage 1 = 512 B ending `0xAA55` and Stage 2 ≤ 32 KiB.

### 1.6 Layer 5 — one-command build wrapper (`build-bootimage.sh`)
* Drives the whole chain for a profile: build CIBOS kernel → flat → `.cimg`;
  build CIBIOS (`firmware-bootloader`) → flat; read the real ELF entry; assemble
  the `.img`. Honestly refuses `maximum-isolation`/`balanced` (need the no_std
  verifier) and i686 (two unrelated gaps), with clear messages.

### 1.7 Console driver — VGA text console (`kernel-image/src/arch/vga.rs`)
* Serial already existed; the gap was on-screen output. Added a no_std VGA
  text-mode console (0xB8000, 80×25, cursor, scroll). The kernel's x86_64 `putc`
  now writes to **both** serial and VGA. aarch64/riscv64 keep serial.

### 1.8 The boot bug — found and fixed by real runtime testing
* Installed QEMU and booted the real disk image. The chain reached CIBIOS and
  "transferring control to CIBOS" but the kernel never ran — a triple fault.
* Diagnosed with the QEMU monitor + `-d int,cpu_reset`: the kernel was correctly
  placed at 16 MiB, but `jump_to_kernel` jumped to the *handoff pointer* (a stack
  address) instead of the entry. **Root cause: an inline-asm register collision**
  — `mov rdi, {handoff}; jmp {entry}` let the allocator put `{entry}` in RDI too.
* Fixed in **all four arches** by pinning the handoff to the arg register
  (`in("rdi")`/`eax`/`x0`/`a0`) and letting `{entry}` use any other register.

### 1.9 Runtime result — it boots
After the fix, `compute` and `performance` images boot fully in QEMU, on **both**
serial and the VGA screen:
```
CIBIOS v0.1.0 starting → … → transferring control to CIBOS
CIBOS kernel: entry
CIBOS kernel: heap online (8388608 bytes)
CIBOS kernel: handoff accepted, 133692416 bytes usable across 1 region(s)
CIBOS kernel: init lane running          (the weighted-entropy scheduler ran a task)
CIBOS kernel: scheduler idle after 1 poll(s)
CIBOS kernel: boot complete
```
The full from-scratch path **BIOS → MBR → Stage 2 → CIBIOS → CIBOS → scheduler**
is runtime-confirmed. This closes review gap **T3-D** ("no directly-bootable USB
image").

### 1.10 Supporting deliverables
* `qemu-boot.sh` — repeatable headless boot test (captures serial + VGA).
* `TESTING-GUIDE.md` — full QEMU + USB testing instructions.
* `BOOTLOADER-DESIGN.md` — design + the runtime findings and the fix.
* Fresh four-arch firmware ELFs and the full sandbox archive.

### 1.11 Verified state at session end
* Host suite **225 / 0**. Clippy clean. `cibios` builds bare on
  x86_64/aarch64/riscv64 (+ i686 via build-std); `kernel-image` builds on
  x86_64/aarch64/riscv64. `compute`/`performance` boot to `boot complete` in QEMU.

### 1.12 Track 2 — live ring-3 multi-context + cross-boundary IPC (later sessions)
Built on top of the boot/runtime baseline above; each increment was kept host-green,
bare-building on all arches, and runtime-verified in QEMU before moving on. Full
detail (per-increment plans, bug-find/fix logs, lock-discipline + memory-safety
analyses, and the production-reality analysis) lives in `TRACK2-LIVE-CONTEXT-DESIGN.md`.

* **Per-lane ring-3 context save/resume (steps 1+2).** `SavedUserContext`
  (`#[repr(C)]`, 160 bytes) + `resume_user.s`: the context-saving trap stub saves a
  trapped lane's FULL register file into `*CURRENT_USER_CTX` (a kernel-set "current
  lane" pointer — arbitrary-lane by construction, not a single slot); `resume_ring3`
  / `resume_user_context` take the context pointer as an argument. Compile-time
  `const` offset guards assert the asm offsets every bare build (proven to fail on
  drift). QEMU-verified: a lane yields, the kernel parks it, then resumes it from the
  exact trap point.
* **Selector-owned `Ring3Table` (step 3).** A per-`LaneId` table `{ctx, boundary,
  started, exited}` driven by the canonical `cibos_kernel::Scheduler` (Ready/Stalled +
  weighted-entropy selection — single selector, no parallel one). QEMU-verified: two
  lanes with distinct boundaries, selector picks one, a lane parks, the other runs,
  the parked one resumes. Lock-safe static-table loop (`run_installed`) holds the
  table lock only briefly, never across `resume_user_context`.
* **`spawn` syscall + real boundary (step 4).** `KernelSyscallEnv::spawn` maps a
  fresh stack into the caller's space (`AddressSpace::adopt`) and registers the new
  lane in the caller's boundary; the trap reads the running lane's REAL boundary
  (`active_lane → boundary_of`) instead of a hardcoded stand-in. No new ABI — the
  dispatcher already routed `Spawn`/`OpenChannel` through `req.boundary`. QEMU-
  verified: a ring-3 app calls `spawn(17)` at runtime, the child lane runs.
  `arg` is marshaled into the spawned lane's `rdi` (verified: child spawned with
  `0x42` exits `0x42`).
* **Cross-boundary channel system unified onto the canonical `Channel`.** Replaced
  the `LocalChannel` stand-in (which ignored boundary) with a boundary-aware handle
  table `(boundary, handle) → Channel`; both endpoints of a cross-boundary channel
  map to the SAME kernel-owned channel, so bytes pass THROUGH the kernel
  (`try_send`/`try_recv`), never via shared user memory. The selector's `Scheduler`
  is shared (`Arc`) as the channels' back-pressure `KernelInterface` — one selector
  for both lane dispatch and channel wakeups.
* **Cross-boundary channel handshake (request / accept-all-or-reject).**
  `ChannelRegistry` request/poll/accept/reject (canonical: terms proposed by the
  requester, accepted wholesale or rejected, point-to-point). Exposed over syscalls
  18–22 (`RequestChannel`/`PollChannelRequest`/`AcceptChannel`/`RejectChannel`/
  `PollChannelOutcome`) with fixed-size wire encodings, plus ring-3 SDK wrappers in
  `cibos-app`. QEMU-verified end-to-end: X(0x100) requests → Y(0x200) accepts → X
  sends `hello-Y` → Y receives the identical bytes; a wrong boundary (0x999) is
  rejected on accept (point-to-point isolation); a channel exists only after the
  target accepts.

**Verified state now:** Host suite **338 / 0**. Clippy clean. `cibios` +
`kernel-image` build bare on x86_64/aarch64/riscv64 (+ i686 via build-std). The
`compute` profile boots in QEMU and runs, in one boot: the cross-boundary handshake
demo, the spawn+arg multi-lane demo, and the normal `.capp` ring-3 app flow, then
`boot complete`. The HIP invariants hold: binary boundary isolation, single selector,
no global locks across user execution, cross-boundary contact only by mutual accept.

---

## PART 2 — EVERYTHING LEFT TO COMPLETE

Organized by the critical path first (each item depends on the ones above it for
a fully usable OS), then the broader documented scope. Status reflects the
alignment review plus this session's progress.

### 2.1 Immediate critical path (to a self-sufficient, interactive OS)

1. **no_std SPHINCS+ verifier** *(unblocks the most)*
   - Today: the SPHINCS+ signature check works in host tooling (`mkimage`) and
     host tests, but `pqcrypto-*` needs `libc`, so it cannot compile into the
     bare firmware. The default firmware is therefore Lightweight (hash-only).
   - Needed: a `no_std`, libc-free SPHINCS+ **verify** (pure-Rust, or a vendored
     freestanding C verifier with a libc shim). Verify-only — no keygen/sign in
     firmware.
   - Unblocks: Standard firmware on bare metal, and the **`maximum-isolation`**
     and **`balanced`** bootable images (which `build-bootimage.sh` currently
     refuses). See `SECURITY-NOTES.md`.

2. **MMU / hardware-enforced isolation** *(the core HIP premise)*
   - **Mechanism DONE and runtime-verified.** Built three layers:
     `cibos-kernel/src/frame.rs` (portable bitmap physical frame allocator, 6
     tests), `cibos-kernel/src/paging.rs` (portable 4-level page-table model with
     a `PageTableEncoder` trait the arch supplies; 7 tests including
     `two_spaces_are_independent` — the core isolation property), and
     `kernel-image/src/arch/paging.rs` (x86_64 entry encoder + `CR3` install).
   - The booted kernel now builds its own page tables through this model and the
     x86_64 encoder, identity-maps physical RAM, and **switches `CR3` to its own
     tables, continuing to execute** — runtime proof in QEMU that the portable
     model produces valid hardware tables (`MMU online — running on kernel-built
     page tables`). aarch64/riscv64 build with a no-op bring-up (arch encoder
     pending; the portable model is shared).
   - **Remaining for full isolation:** *(per-container address spaces now DONE
     and runtime-verified.)* `cibos-kernel/src/address_space.rs` provides an
     `AddressSpaceManager` giving each `BoundaryId` its own `AddressSpace` /
     page-table tree, with create/map/translate/destroy and data-frame
     reclamation on teardown (5 host tests, including `boundaries_are_isolated`).
     At boot the kernel builds two distinct boundaries on the live MMU and
     confirms a page mapped in boundary 1 is **physically absent** from boundary
     2 (`container isolation verified — … separate page tables`). So
     "Container A cannot read Container B" is now hardware-enforced per boundary.
   - **Still to do here:** wire `AddressSpaceManager` into the `Kernel` struct so
     container create/destroy in `ContainerRegistry` automatically create/destroy
     the matching address space (today the manager is exercised in the boot
     demonstration and fully unit-tested, but not yet a held `Kernel` field);
     reclaim page-table-node frames on teardown (data frames already reclaimed);
     and add an aarch64 `PageTableEncoder` (TTBR0/long-format descriptors) to
     bring the same enforcement to ARM (the portable manager is already shared).

3. **Application loader on the booted kernel** *(currently nothing runs as an app)*
   - **Syscall transport DONE and runtime-verified** (the kernel⇄app boundary
     mechanism). New `shared/src/protocols/syscall.rs` (the ABI: numbers,
     register convention, error codes; 2 tests), `cibos-kernel/src/syscall.rs`
     (portable dispatcher over a `SyscallEnv`; 7 tests — `Log`/`Exit`/`Yield`/
   - **Ring-3 user-mode execution + minimal loader DONE and runtime-verified.**
     `kernel-image/src/arch/gdt.rs` (kernel GDT with ring-3 code/data selectors +
     a TSS with rsp0 and an IST fault stack), `arch/enter_user.s` (`iretq` into
     ring 3), `arch/user_payload.s` (a position-independent unprivileged payload),
     `arch/idt.rs` + `arch/syscall_entry.s` (now also CPU-exception handlers for
     vectors 0–19 on an IST), and `loader.rs` (`run_user_payload`: map a user
     code page + stack into an `AddressSpace` with user perms, drop to ring 3).
     At boot the kernel enables EFER.NXE, builds its tables, drops to ring 3, and
     the unprivileged payload prints via an `int 0x80` `log` syscall, then
     `exit(0)` cleanly halts: `[ring3] hello from an unprivileged user payload
     via int 0x80` → `user payload exited with code 0`.
     - *Debugging note for the future timer work:* the legacy 8259 PIC is at BIOS
       defaults (IRQ0 → vector 0x08), so the first time `iretq` sets RFLAGS.IF a
       timer tick is delivered as a spurious "double fault." The kernel masks the
       PIC (`arch::mask_pic`) before entering ring 3 since it has no timer/IRQ
       driver yet; a real timer/APIC subsystem must remap and handle these.
   - **Process model — context save/restore DONE and runtime-verified.** Added
     `enter_user_context`/`return_to_kernel` (`arch/enter_user.s`): a
     setjmp/longjmp-style pair across the privilege boundary. The kernel saves
     its callee-saved registers + RSP before `iretq`, and a user `exit` syscall
     restores that context so control unwinds back to the kernel instead of
     halting. `loader::run_user_payload_returning` uses it; the demo now shows
     `[ring3] hello … → user payload returned to kernel with exit code 0 → boot
     complete`. The full vertical slice works: boot → kernel → MMU → isolation →
     ring-3 user process → syscall → exit → return to kernel → completion.
   - **Still to do for a *full* process model:** (a) load an external application
     image (parse format, map segments with per-segment perms, relocate) — reuses
     the loader's map+entry path, only the byte source changes; (b) run each app
     in its *own* `AddressSpace` (installed via CR3 on entry) so traps carry its
     real `BoundaryId` and `copy_from_user` goes through `AddressSpaceManager`
     rather than the identity read; (c) preemptive multitasking — a timer/APIC
     driver (which must remap the PIC, currently masked) plus per-task context
     save so the scheduler can switch between several ring-3 tasks; (d) grow the
     ABI to marshal the rest of the SDK `System` surface (channels, spawn,
     sleep).

4. **Syscall transport** *(kernel⇄app boundary)* — **mechanism DONE**, now
   exercised from real ring-3 code (see item 3). Remaining is breadth: more
   syscall numbers as `System` operations move onto the transport, and per-task
   context save/restore for preemptive multitasking.

5. **Documented behavioral flags — implement (declared but inert)**
   - The profile bundles exist and select flags, but most flags gate no behavior
     yet. Per the review's table, implement: `anti-starvation` (ready-pool
     wait-time tracking), `full-fairness`, `weight-aging`, `class-core-affinity`,
     `class-resource-pools`, `signal-coalescence` (+threshold), `rtro`,
     `cryptographic-ipc` (as a real distinct mode), `lightweight-handshake`,
     `multi-user-isolation`, `audit-logging`. These are the substance behind
     "Maximum Isolation vs Compute" being genuinely different binaries (ADR-007).

6. **The 8 documented examples** *(canonical API conformance suite)*
   - Build `hello-lane`, `channel-communication`, `parallel-computation`,
     `pipeline-processing`, `compute-intensive`, `event-driven-ui`,
     `mobile-sensor`, `profile-flexible`. They validate the application API and
     the profile system. (Review item T2-B.)

7. **Input on the booted kernel**
   - Keyboard/mouse (and later touch) wired into the booted kernel so it is
     interactive (a shell/login surface). Today there is no input at the kernel.
   - **DONE (keyboard).** IRQ1 → scancode decode → key queue → blocking `ReadKey`
     syscall (sleeps the CPU via `hlt`, waits INDEFINITELY for a live keystroke);
     `read_line` consumes it. Verified live in QEMU (`sendkey`): typed input
     reaches the login app. Mouse/touch remain for the GUI/mobile surfaces.

8. **A shell / login / first interactive surface**
   - The booted kernel currently prints `boot complete` and idles. A minimal
     interactive surface (CLI shell, then the login flow) makes it usable.
   - **DONE.** `login-rs` + `shell-rs` (.capps) run the real shared `login`/
     `accounts` gate and `shell::dispatch` over the actual package-manager/kvstore/
     editor/trove crates. Boot → login → GATED shell (shell only on a granted
     login) → app-store install → exit, with credentials persisted to CIBOSFS.
     Available two ways: a deterministic INJECTED path (`storage-selftest`, for
     regression) and a LIVE path (`interactive-session`, real keyboard). Both
     runtime-verified in QEMU. (See `F1-INTERACTIVE-SESSION-PLAN.md`.)

### 2.2 Capability subsystems (documented flags, all absent — review T3-A)

Each is a documented capability and gates one or more profiles (Mobile especially):
- **Real display driver** (beyond VGA text: framebuffer/graphics modes; the
  `platform-gui` `Surface` is a cell-grid model, not a driver).
- **USB device stack** (`usb-stack`).
- **Audio subsystem** (`audio-subsystem`).
- **Sensor subsystem** (camera/mic/GPS with per-sensor isolation —
  `sensor-subsystem`).
- **Power management** (`power-management`).
- **Mobile connectivity** (`mobile-connectivity`).

### 2.3 Networking to real hardware (review T3-B)

- Today: the networking stack (the invented Lattice/Gate/Link/Warden/Probe/Vane/
  Lens/Hail vocabulary) is **in-process loopback only**; there is no NIC.
- Needed: a NIC driver + packet transport beneath the same API so a browser can
  actually reach the internet. (Requires hardware to validate — correctly a
  boundary, not a faked feature.)

### 2.4 Storage & persistence

- Today: a `storage/` crate exists but is not a booted, writing filesystem; the
  boot path mounts nothing and nothing persists between boots.
- Needed: a block/storage driver wired into boot, a filesystem the kernel mounts,
  and a persistence story (this is the prerequisite for "persistent USB" and any
  install-to-disk beyond raw-image live boot).

### 2.5 Boot/install breadth

- **UEFI boot loader** — today is legacy BIOS / CSM only. A UEFI path is needed
  for modern machines without legacy support.
- **i686 (32-bit) bootable image** — two gaps to close (review-adjacent):
  (a) `mkimage` cannot stamp the 32-bit `x86` arch tag; (b) `kernel-image`'s
  32-bit x86 backend is incomplete (missing `arch::putc`/`init_serial`/`halt`,
  two type mismatches). The bootloader/contract/mkbootimage/CIBIOS i686 paths are
  already done.
- **Partitioned disk image + installer** — the current image is a raw
  "superfloppy" (no partition table). A guided installer (partition, copy,
  configure) and a persistent layout are future work.
- **ARM/RISC-V flashable images** — currently those boot only via QEMU `virt`
  device tree; real-hardware boot images for them are future work.

### 2.6 Mobile / phone bring-up (not started)

- ARM phone boot mechanism (device tree / Android boot image / vendor
  fastboot flow) — the from-scratch bootloader here is x86 BIOS only.
- Mobile display/touch/connectivity drivers in the booted kernel.
- Phone-flashing tooling.
- Depends on: 2.1 (isolation, loader), 2.2 (display/touch/connectivity), and an
  ARM boot image target.

### 2.7 Profile/feature system completeness (review T1 keystone)

- The four profile **bundles** now exist and select flags (done in an earlier
  phase). What remains: the **feature interaction matrix** (incompatibilities,
  dependencies, shared timing infrastructure) and ensuring every prohibited
  feature is genuinely *absent* from each profile binary (not runtime-disabled) —
  the backing for the "Quantum-Like First, Security Optional" guarantee.

### 2.8 Scope-creep triage (review item, deferred by your call)

- 17 applications exist (`trove`, `courier`, `postbox`, `contacts`, `calendar`,
  `clock`, `lens`, `vane`, `web-protocol`, `probe`, `notepad`, `shell`, `editor`,
  `kvstore`, `calc-service`, `package-manager`, `lockscreen`) vs the 8 documented
  examples. After the core aligns: keep what maps to a real need, re-vocabulary
  what drifted (the invented networking names), shelve the rest — deliberately,
  on your call.

### 2.9 End-to-end hardware validation (the deferred feedback loop)

- Everything bare-metal beyond what QEMU exercises (real NIC, USB, sensors,
  audio, display modes, power, phone hardware) needs validation on physical
  devices. This was accepted as the final step; QEMU has now proven the boot
  chain, so the loop has started.

---

## PART 3 — SUGGESTED NEXT ORDER

1. no_std SPHINCS+ verifier → unlocks Standard firmware + the two remaining bare
   profiles (small, high leverage, all-host-testable).
2. MMU/hardware isolation → makes the central HIP guarantee real.
3. App loader + syscall transport → things actually run on the kernel.
4. The 8 examples → validate the API and the profile system on the real kernel.
5. Behavioral-flag implementations → make the profiles genuinely differ.
6. Input + a shell → first interactive, usable surface.
7. Then breadth: display/USB/audio/sensors/power, NIC, storage/persistence,
   UEFI, i686, installer, ARM images, and finally mobile.

Each step keeps the host suite green and the bare targets building, verifies in
QEMU where possible, and defers only what genuinely needs physical hardware —
the same discipline used this session.
