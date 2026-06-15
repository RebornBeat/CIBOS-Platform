# CIBOS forward plan — display, System/IPC, network, orchestrator, i686, full flow

## STATUS UPDATE (executed this arc — assessment-first, all verified)
* TRACK 2 (System/IPC) — FOUNDATION done: assessment found the kernel ALREADY
  runs a LaneExecutor + scheduler and spawns cooperative async lanes at boot
  (boot.rs `kernel.spawn(...)` + `run_until_idle()`, proven by "init lane
  running"). So the multi-context machinery exists at the kernel level; the gap
  is the ring-3 bridge. First ring-3 syscall onto that surface added: `Sleep`
  (= 13). Full path wired + tested: shared Syscall enum + from_number; SyscallEnv
  `sleep_nanos` (default no-op) + dispatch arm; `KernelSyscallEnv::sleep_nanos`
  backed by the PIT monotonic counter (sti;hlt idle-wait); `cibos_app::sleep_nanos
  /sleep_millis` ring-3 wrappers. 319/0 (sleep_returns_ok test); all four arches
  build; clippy clean. TRACK 2 REMAINING: `OpenChannel`/`Spawn` syscalls (same
  extension pattern) + the cooperative model to run MULTIPLE ring-3 contexts
  (apps still run via single synchronous run_app_image_isolated) — the genuinely
  larger piece, bridging ring-3 tasks to the existing kernel executor/channels.
* F1 (interactive session — GATING done): the login->shell flow is now GATED on
  the login result, not two independent demos. The login `.capp` returns 0 on
  GRANTED / 1 on DENIED; boot.rs captures that and launches the shell session
  ONLY when granted. RUNTIME-VERIFIED both ways: correct password -> "login
  GRANTED — starting session" -> shell session runs -> "shell session ended";
  wrong password -> "access denied" and the shell NEVER runs (the gate holds).
  318/0. F1-REMAINING: a fully LIVE interactive session (read the real keyboard,
  loop indefinitely, logout->relogin) rather than the deterministic injected
  command script — the control flow is now correct; removing the test scaffold
  from the default path is the remaining polish.
* TRACK 1 (display driver) — DONE + RUNTIME-VERIFIED. Added `vga::put_cell`
  (low-level char+attr write) + `vga::width/height`, and a kernel `gui` runner
  (kernel-image/src/gui.rs) that blits a `platform-gui` Surface to the VGA text
  console (Cell -> u16 char+attr, Color -> VGA 4-bit) and drives a `GuiApp` with
  the existing PS/2 keyboard (poll_key -> InputEvent::Key), mirroring the host
  GuiRunner loop. Ported `notepad` (GuiApp) to no_std (host gui-demo bin gated
  behind a `std` feature). A `gui-demo` kernel feature runs notepad on boot.
  PROOF: dumped physical VGA memory at 0xB8000 via QMP pmemsave after boot — the
  buffer held notepad's actual render ("CIBOS Notepad" / "hello cibos" / caret /
  hint line), confirming the full Surface->VGA blit chain on emulated hardware.
  Default build has zero warnings (gui gated behind the feature); all four arches
  build the kernel; 318/0.
* QUICK FIXES (Part E step 1) — ALL DONE: (1) mkimage now accepts the `x86`
  (i686) arch tag; (2) ONE `ShellFs::list` contract = immediate child names,
  honored by BOTH backends (host SDK rewritten to derive child names; kernel
  already returned names) — added `Filesystem::all_keys()` for whole-FS ops
  (storage serialization/zeroization) that genuinely need full paths, and fixed
  contacts/calendar to the new contract; (3) `ShellFs::exists` default now also
  detects directories (via list). 318/0; storage 6/6 after the all_keys split.
* i686 RUNTIME (Track 5) — DONE for the kernel-boot milestone: the i686 BIOS .img
  now builds through `build-bootimage.sh compute i686` (nightly+build-std wired
  via an I686 flag; TNAME derives the cargo output dir for the custom JSON
  target) and BOOTS in qemu-system-i386: CIBIOS (32-bit) -> image found/verified
  -> handoff -> kernel entry -> heap -> scheduler -> boot complete. ALL FOUR
  ARCHES NOW BOOT THE KERNEL. x86_64 still builds+runs the full login/shell/store
  flow through the modified script (no regression). i686-REMAINING: serial-only
  (no VGA yet) + MMU/paging bring-up pending, so i686 boots the kernel but does
  not yet run the ring-3 app flow (shared with F2 below).

Remaining tracks (1 display, 2 System/IPC, 3 network, 4 orchestrator) and the
full-flow items (F1 interactive session, F2 per-arch app flow, F3 TUI, F4 PIN,
F5 deploy/audit) are unchanged below.

--------------------------------------------------------------------------------

Review-only capture (no code edited). This is the locked concept for the next
build arcs, plus every bug/discrepancy found by reading. Read alongside
PORT-PLAN-AND-REVIEW.md (authoritative ordered plan), NETWORKING.md (the Lattice),
PLATFORMS.md, BOOT.md, SECURITY-NOTES.md.

Ground rule for all of it: assessment-first, bare-metal-first, reuse existing
primitives verbatim, no placeholders, verify each increment (host + bare build,
tests, clippy, QEMU) before claiming done. Toolchain/QEMU do NOT survive a
sandbox reset — only the outputs archive does; this review was done statically by
reading the extracted tree (no compiler/emulator this turn, which is correct for
a no-edit review).

================================================================================
## PART A — BUGS / DISCREPANCIES FOUND BY READING (fix as part of the work)
================================================================================

1. i686 image-packaging blocker (REMAINING, small). `tools/mkimage/src/main.rs`
   `arch_from_str` accepts only x86_64/aarch64/riscv64 — it cannot stamp the `x86`
   tag (ProcessorArchitecture::X86 = 3), so CIBIOS rejects an i686 `.cimg` as
   wrong-arch. FIX: add `"x86" => Some(ProcessorArchitecture::X86)`, then re-add
   i686 to the build loop. (Blocker #2 from build-bootimage.sh's header — the
   kernel arch backend missing putc/init_serial/halt + linker — is now STALE:
   fixed last arc. Only this mkimage tag + the runtime handoff/boot path remain.)

2. `ShellFs::list` semantics DIVERGE between backends (latent, blast radius 4
   apps). Host SDK `Filesystem::list(prefix)` returns FULL KEYS that start with
   the prefix, RECURSIVELY (flat BTreeMap). Kernel `SyscallFs::list` →
   `fs::list` → CIBOSFS `list_dir` returns IMMEDIATE CHILD NAMES, non-recursive.
   `list` is called by shell, trove, contacts, calendar. trove was patched to
   tolerate both; the others will break silently when ported. FIX: define ONE
   `ShellFs::list` contract (recommended: immediate child names of the directory)
   and make BOTH backends honor it (adjust the host SDK to return child names, or
   document recursion explicitly). Do this BEFORE porting contacts/calendar.

3. Login/shell are SCRIPTED DEMOS, not a gated interactive session (structural).
   In `kernel-image/src/boot.rs` the login-create, login-auth, and shell `.capp`s
   run SEQUENTIALLY and UNCONDITIONALLY with injected keystrokes (alice / pw123 /
   store browse…). There is no `if login == Granted { run shell }`, no wait for a
   human, no persistent session loop. So the real product flow (boot → login
   prompt → wait → on success enter interactive shell → stay) is NOT yet wired;
   what exists is a runtime PROOF that each piece works. This is the central item
   for "full boot/login/UI-UX".

4. `ShellFs::exists` default is wrong for directories (minor). Default
   `exists(path) = read(path).is_some()` returns FALSE for a directory (can't read
   a dir as a file). Kernel SyscallFs and the SDK both OVERRIDE with a native
   exists (correct), but any future ShellFs impl relying on the default would
   mis-report directory existence. FIX: note the contract, or give the default a
   safer probe.

5. i686 kernel is SERIAL-ONLY (no VGA) (note, not a bug). `kernel-image/src/arch/
   x86.rs` drives COM1 only; x86_64 layers VGA text on top. So even once i686
   boots it has no on-screen output until a VGA path is added — separate from the
   display-driver track below. The firmware↔kernel i686 handoff convention itself
   is CONSISTENT (firmware `jump_to_kernel` passes handoff in EAX + bare jmp; the
   kernel `boot/x86.s` reads EAX, cdecl-pushes the u64, calls kernel_entry).

================================================================================
## PART B — PER-PLATFORM / PER-ARCH BOOT + FLOW REALITY (confirmed by reading)
================================================================================

ARCHES (ProcessorArchitecture enum = 4): x86_64, aarch64, riscv64, x86 (i686).

* x86_64 — FULL boot proven. `build-bootimage.sh` makes a complete BIOS `.img`
  (stage2 → CIBIOS → verify → kernel). Product flow runtime-verified: persistent
  CIBOSFS → login (create+auth) → shell in ring 3 → compose pkg/kv/edit/store →
  install from local repo. VGA text + PS/2 keyboard present.
* aarch64 / riscv64 — boot via QEMU `-kernel cibios -initrd .cimg` (no BIOS
  image; CIBIOS reads the DTB `/chosen` initrd pointer; handoff x0/a1). Firmware
  + kernel build (build-profile.sh, entry 0x41000000 / 0x81000000). Serial
  console only (no framebuffer console yet). The app layer is x86_64-first; the
  login/shell `.capp` run blocks in boot.rs are `#[cfg(target_arch="x86_64")]`-
  gated, so aarch64/riscv64 prove firmware→kernel handoff + liveness but do NOT
  yet run the app/login/shell flow.
* i686 — firmware builds (custom target, build-i686.sh). Kernel now COMPILES AND
  LINKS (this arc). NOT runtime-proven: needs mkimage `x86` tag (bug #1), a BIOS
  image path (build-bootimage.sh is x86_64-only today), and a QEMU i686 boot run.

PLATFORMS (4 crates):
* platform-cli — DONE, the Console seam; the on-kernel shell uses it.
* platform-gui — no_std, builds bare on all 3 arches. Character-cell Surface +
  GuiApp. NO kernel display driver yet (see track 1).
* platform-mobile — no_std, builds bare on all 3 arches. Touch/gesture over the
  GUI Surface. Needs the display driver + a touch input source.
* platform-server — STILL host/std + SDK-coupled (6 std/SDK refs). Needs the
  kernel System (track 2) before it runs on the kernel.

APPS (17) by surface (the correct frame — NOT "all 17 on the shell"):
* CLI/shell (process_command/CliApp): shell, package-manager, kvstore, editor,
  trove = 5 DONE on the kernel shell. calc-service (CliApp + open_channel/spawn)
  and probe (CliApp + handle/spawn) are CLI but need kernel IPC → wait on track 2.
* GUI (GuiApp render/handle): notepad → waits on track 1.
* Touch (TouchApp on_gesture): lockscreen, lens (render + spawn) → track 1 (+ IPC
  for lens).
* Library/data (no standalone entry; backing logic other apps compose): calendar,
  clock (Stopwatch), contacts, courier (Message/Inbox), postbox (Mail/Mailbox),
  web-protocol (Request/Response — transport-agnostic), vane (channels). These are
  ported when the app that composes them is, not as shell programs.

================================================================================
## PART C — THE FIVE WORK TRACKS (scope, where it plugs in, increments)
================================================================================

Each track was scoped by reading the existing code; estimates reflect how much
already exists.

### TRACK 1 — Kernel display driver (unblocks GUI: notepad, lockscreen, lens)
SMALLER than expected. The GUI `Surface` is a CHARACTER-CELL grid (`Cell{ch,
color}`), and the kernel ALREADY has a VGA text console at 0xB8000 (80×25, 16
colors) in `kernel-image/src/arch/vga.rs`. So the driver is a Surface→VGA blit
(each Cell → one VGA u16 char+attr), NOT a framebuffer/pixel/graphics stack. The
ASCII/TUI UX is by design (PLATFORMS.md). Input is the existing PS/2 keyboard
(ReadKey syscall); touch (mobile) needs a pointer event source (later).
Increments: (1a) a `Display` seam exposing the Surface to ring-3 (a syscall to
blit a cell region, or a kernel-side GuiRunner that owns the Surface and the
keyboard). (1b) port ONE GuiApp (notepad) no_std; render to VGA; drive with the
keyboard. (1c) lockscreen (TouchApp) once a pointer source exists. Runtime-verify
each in QEMU (serial + observable VGA state).
NOTE: x86_64 has VGA; aarch64/riscv64/i686 need their own text-console path for
on-screen GUI (serial-only today) — track per arch.

### TRACK 2 — Kernel System / IPC (unblocks calc-service, probe, vane, server)
SMALLER core than expected (scheduler/channels EXIST), but with one genuinely
larger piece. The kernel already has `cibos-kernel/src/scheduler.rs` (register_
lane / notify_complete / take_dispatch_batch / weight classes), `channel.rs`
(try_send/try_recv/send/recv/Channel), container/selector, and a full
`cibos-async-runtime` (executor/future/waker). The GAP: none of this is exposed
to ring-3. The syscall ABI stops at 12 (FsDelete); the kernel `SyscallSystem`
exposes only filesystem/now_nanos/resource_limits, while the SDK `System` also
has sleep/open_channel/spawn/spawn_with_lane/spawn_user/lattice/boundary/
check_allocation.
Increments: (2a) add Spawn / OpenChannel / Sleep syscalls bridging ring-3 to the
existing kernel scheduler/channels. (2b) THE LARGER PIECE: a model to run
multiple ring-3 contexts cooperatively — today apps run via
`run_app_image_isolated` = a single synchronous run-to-completion; concurrency/
IPC from userspace needs a cooperative multi-context executor on the ring-3 side
+ kernel scheduling. (2c) extend `SyscallSystem` to implement the full
ShellSystem/System surface. (2d) port calc-service + probe (CLI + IPC) and
runtime-verify. This track also underlies networking (Lattice ≈ channels) and
platform-server.

### TRACK 3 — Network stack (unblocks web-protocol, courier, postbox, lens)
The "Lattice" (NETWORKING.md) is the designed net layer: Gate (u16 endpoint),
Link (bidir byte stream), Warden (firewall), Probe (scanner), via
`system.lattice()`. The CURRENT Lattice is an in-memory loopback fabric that
WORKS today; "apps written against the Lattice won't change when a NIC is added".
web-protocol is PURE transport-agnostic logic (no sockets); there is NO NIC
driver. KEY consequence: network apps do NOT need a NIC to run on the kernel —
they need the Lattice exposed via syscalls (≈ open_channel from track 2) over
loopback first; the NIC transport is the LAST layer.
Increments (NETWORKING.md roadmap): (3a) expose Lattice to ring-3 over loopback
(rides track 2). (3b) vane — request-serving daemon (binds a Gate, serves FS
content). (3c) lens — client/browser (connects, renders). (3d) named request
protocol over Links (the HTTP equivalent); courier/postbox over it. (3e)
Gate-ownership by boundary (Warden per-boundary policy). (3f) NIC-backed
transport beneath the Lattice (real off-machine) — LAST.

### TRACK 4 — Server orchestrator (NEW app — genuinely missing)
A "Proxmox-VE-for-CIBOS": manage CIBOS instances/containers/profiles, provision,
with isolation boundaries first-class. No such app exists. platform-server is its
host (needs track 2). Increments: (4a) design the orchestrator's model (what it
manages: instances, containers, profiles, boundaries) — a design doc first. (4b)
implement over the kernel System (channels/spawn) + the Warden/boundary model.
(4c) a CLI/GUI surface for it. Depends on tracks 2 (System) and 3 (Lattice).

### TRACK 5 — i686 firmware↔kernel runtime handoff + QEMU boot path
The kernel builds/links for i686; the handoff register convention is consistent
(EAX). Increments: (5a) mkimage `x86` tag (bug #1). (5b) a BIOS image path for
i686 (build-bootimage.sh is x86_64-only; either generalize it or add an i686
variant using stage2-i686 + the i686 firmware + the i686 kernel). (5c) QEMU
i686 boot run; confirm firmware verify → kernel entry → serial liveness. (5d)
(optional) i686 VGA text path for on-screen output. NOTE i686 is serial-only at
the kernel today.

================================================================================
## PART D — FULL-FLOW / UI-UX COMPLETION (cross-cutting, needed for "it boots,
## you log in, apps run" on every platform/arch)
================================================================================

F1. Interactive session (bug #3): replace the scripted login+shell demos with a
    real flow — boot → login prompt → wait for human input → on Granted, launch
    the shell as an interactive session that persists (loop reading commands)
    until logout/exit → return to login. Gate the shell on the login result.
F2. Per-arch app flow: the login/shell `.capp` run path is x86_64-only. Bring the
    app/login/shell flow to aarch64/riscv64 (and i686) — at least over serial
    first, then their display path. Requires the `.capp` build + boot.rs run
    blocks to be arch-general (today they are `#[cfg(target_arch="x86_64")]`).
F3. UI/UX polish (ASCII/TUI): the GUI is text-cell by design; define a consistent
    TUI look (prompts, menus, the lockscreen, notepad) for both CLI and GUI
    surfaces. Mobile = touch gestures over the same Surface + a PIN/passphrase
    lockscreen.
F4. Mobile auth (PIN): `accounts` already supports password verifiers; a PIN is a
    short-credential policy over the SAME bridge — small add, not a new system.
F5. Deploy/security UX (own track, audit-first; see SECURITY-NOTES.md): per-image
    embedded trusted key (each `.img` sensitive, not WWW-shareable); quantum vs
    non-quantum selection (firmware now DISPATCHES on the image's
    signature_algorithm with a tested fail-closed guarantee — SPHINCS+ is the
    bare-verifiable default; ML-DSA needs a portable verifier; Ed25519
    intentionally unavailable). Remaining: USB-flash deploy UX (install-to-
    partition choice, persistent-vs-live at install), and a zero-day/isolation
    audit pass (the isolation core — profiles, ring-3, per-process address spaces
    — is built and the kernel prints "container isolation verified", but a
    deliberate audit is its own track).

================================================================================
## PART E — RECOMMENDED ORDER (dependency-correct, least-drift)
================================================================================

1. Quick correctness fixes first (small, unblock the rest, prevent silent bugs):
   bug #1 (mkimage x86 tag), bug #2 (ShellFs::list single contract), bug #4
   (exists default). These are surgical.
2. TRACK 1 (display driver) — smallest, high-visibility, unblocks GUI apps; makes
   "apps run with UI" real on x86_64. Pairs with F1 (interactive session) so the
   shell/GUI actually waits for and serves a human.
3. TRACK 2 (System/IPC) — the keystone: unblocks calc-service/probe/vane,
   networking (track 3 rides it), and platform-server. Do the syscall bridge
   first, then the cooperative multi-context model.
4. TRACK 3 (network over loopback) — vane → lens → request protocol →
   courier/postbox. NIC transport LAST.
5. TRACK 5 (i686 runtime) — can be slotted earlier opportunistically (bug #1 is
   step 1 anyway); finish the BIOS/QEMU path when convenient.
6. TRACK 4 (server orchestrator) — design doc, then build on tracks 2+3.
7. F2/F3/F4 (per-arch flow, TUI polish, mobile PIN) folded into the tracks as each
   surface comes up. F5 (deploy/audit) as a dedicated late track.

Throughout: keep `process_command`/app logic PURE and reuse verbatim; put
backend coupling behind seams (ShellFs/ShellSystem/Console/Display/Lattice);
gate std-only bits behind features; verify host+bare+clippy+QEMU per increment;
update PORT-PLAN-AND-REVIEW.md as the authoritative tracker; archive FROM
/home/claude/cibos-workspace keeping only the latest in outputs.
