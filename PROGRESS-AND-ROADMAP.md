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
   - Today: isolation is **accounting only** — the kernel tracks ownership and
     resource use, but there are no page tables, no `cr3`/`satp`/`ttbr`
     programming. "Container A cannot read Container B" is not yet physically
     enforced.
   - Needed: per-container page tables / address spaces, set up by CIBIOS before
     CIBOS runs and maintained by the kernel; the boundary model from HIP made
     real in hardware. (Review item T1.)

3. **Application loader on the booted kernel** *(currently nothing runs as an app)*
   - Today: the booted kernel runs an internal init lane and idles; there is a
     host-simulation SDK but no on-kernel app execution.
   - Needed: load/start a real application image inside an isolated container on
     the booted kernel; the bridge from the SDK model to actual on-kernel
     processes. (Review item, "on-kernel execution" / Phase 3.)

4. **Syscall transport** *(kernel⇄app boundary)*
   - Needed: the real syscall/trap mechanism connecting applications to the
     kernel core (the channel/lane/IPC vocabulary exists as types; the actual
     trap transport on hardware does not).

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

8. **A shell / login / first interactive surface**
   - The booted kernel currently prints `boot complete` and idles. A minimal
     interactive surface (CLI shell, then the login flow) makes it usable.

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
