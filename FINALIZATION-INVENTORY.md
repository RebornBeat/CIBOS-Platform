# CIBOS / CIBIOS / HIP — Finalization Master Inventory

Purpose: a single, accurate, cross-referenced capture of EVERYTHING — every app
and capability, its true state, where it runs (host vs on-kernel), per-platform
and per-arch coverage, alignment with the canonical 18-doc vision, and the
complete list of gaps + new findings to finalize the whole system. Written
review-only (no code changed) as the map we finalize against. Discipline:
nothing claimed done that is not; honest hardware boundaries are flagged, not faked.

Status anchors (true, verified this session): **338 host tests / 0 failing**;
`cibios` + `kernel-image` build bare on x86_64/aarch64/riscv64 (+ i686 build-std);
the `compute` profile boots in QEMU and runs ring-3 apps, the cross-boundary
channel handshake, spawn, and the gated login→shell session.

---

## 0. THE DRIVER-MODEL QUESTION (resolved, and it shapes Track 3)

A real OS driver targets a **standardized hardware-interface contract**, not one
physical card. virtio-net, e1000, AHCI, xHCI are real, ubiquitous interfaces; a
driver for them is a REAL driver, not a fake — it does not "know" whether it runs
on QEMU, a cloud hypervisor, or bare metal with SR-IOV. The project ALREADY
embodies this: `cibos-kernel` defines a portable `BlockDevice` trait, and
`kernel-image/src/arch/ata.rs` is the concrete ATA/IDE driver behind it. CIBOSFS
runs on that, runtime-verified in QEMU.

Therefore the faithful networking path has TWO faithful layers, not one:
  1. **The Lattice surface on the kernel over syscalls** (Gate/Link/Warden),
     backed by loopback — the stable model a NIC slots beneath (TRACK3-LATTICE-PLAN.md).
  2. **A real `NetDevice` trait + a virtio-net driver** behind it — the SAME
     pattern as BlockDevice/ATA, runtime-verifiable in QEMU (virtio-net is a real
     interface), NOT a fake. e1000 can follow as a second concrete driver.
Both are in-discipline. The earlier "NIC can't be faithful in QEMU" framing was
too strong: virtio-net IS a real interface. (e1000/virtio choice is a driver
detail; the trait keeps apps unchanged — exactly the BlockDevice precedent.)

---

## 1. COMPONENT INVENTORY (crates) — what exists, true state

### Core system (all real, bare-building, tested)
| Crate | Role | State |
|---|---|---|
| `cibios` | Firmware (BIOS replacement); from-scratch boot, SPHINCS+ verify | DONE; boots 4 arches |
| `cibos-kernel` | Microkernel: scheduler, channels, gates(planned), paging, frame alloc, CIBOSFS, BlockDevice, entropy, sync | DONE (the verified core) |
| `kernel-image` | The bootable kernel binary + arch (gdt/idt/paging/ata/asm) + ring-3 loader + demos + on-kernel apps | DONE; boots+runs in QEMU |
| `shared` | Types/protocols (syscall ABI, ipc, handoff, isolation, crypto, hardware) | DONE |
| `cibos-async-runtime` | HIP-native async runtime (.await → Catch-and-Release; no Tokio) | DONE |
| `cibos-sync` | Locks/primitives (SpinLock etc.) | DONE |
| `cibos-macros` | proc-macros (e.g. `#[cibos_sdk::main]`) | DONE |
| `cibos-input` | Portable key/pointer/gesture model + key queue | DONE |
| `cibos-console` | Console abstraction | DONE |
| `storage` | Live (RAM) + Persistent (partition) volume model | DONE (host model; CIBOSFS backs persistent) |
| `accounts` | Profile registry + salted credential records | DONE |
| `login` | Shared auth gate (run_login / run_login_for) | DONE |

### App-facing runtimes
| Crate | Role | State |
|---|---|---|
| `cibos-sdk` | High-level host SDK: AppHost, Lane, Timer, Channel, **Lattice net**, MultiContainerHost, broker | DONE (host) |
| `cibos-app` | `no_std` ring-3 runtime a `.capp` links against: console, fs, input, rand, channels(+handshake), spawn | DONE (on-kernel) |

### Platforms (the four canonical surfaces)
| Crate | Surface | Input | State |
|---|---|---|---|
| `platform-cli` | line console | typed lines | DONE (host runner) |
| `platform-gui` | cell-grid `Surface` | keyboard + pointer | DONE (host runner; Surface inspectable) |
| `platform-mobile` | cell-grid `Surface` | touch gestures (Tap/Swipe) | DONE (host runner) |
| `platform-server` | none (headless) | none | DONE (host runner) |

NOTE: all four platform RUNNERS host apps via the SDK `AppHost` (in-process
kernel). The hardware display driver renders the SAME `Surface` to a framebuffer
(the BlockDevice/NetDevice honest pattern: portable surface + concrete driver).

---

## 2. APPLICATION INVENTORY (17 apps) — state + where they run

All 17 are REAL and unit-tested as HOST programs via `AppHost`. Only 3 are also
built as on-kernel `.capp`s today (hello, login, shell). "On-kernel" = runs in
ring-3 on the booted microkernel.

| App | Role (canonical equivalent) | Host | On-kernel `.capp` | Notes |
|---|---|---|---|---|
| `shell` | command shell | ✅ | ✅ | dispatches pkg/kv/edit/store; live + injected |
| `login` (+`lockscreen`) | auth gate / lock | ✅ | ✅ (login) | CIBOSFS-persisted credentials |
| `package-manager` | apt/dnf equivalent | ✅ | ✅ (via shell `pkg`) | repo install + integrity verify |
| `trove` | app store | ✅ | ✅ (via shell `store`) | installs to /apps |
| `editor` | line/text editor | ✅ | ✅ (via shell `edit`) | |
| `kvstore` | key-value store | ✅ | ✅ (via shell `kv`) | |
| `notepad` | GUI notepad | ✅ | ❌ | GUI (cell-grid) app |
| `calc-service` | calculator IPC service | ✅ | ❌ | channel/IPC demo |
| `clock` | clock | ✅ | ❌ | |
| `calendar` | calendar | ✅ | ❌ | |
| `contacts` | contacts | ✅ | ❌ | |
| `courier` | messaging (over Lattice) | ✅ | ❌ | uses Warden-gated Gate |
| `postbox` | email | ✅ | ❌ | |
| `vane` | server daemon (nginx role) | ✅ | ❌ | binds a Gate, serves /www over Hail |
| `lens` | browser/fetch client | ✅ | ❌ | renders HTML as TEXT (no DOM/JS) |
| `web-protocol` (Hail) | HTTP-equivalent protocol | ✅ | ❌ | request/response over Links |
| `probe` | port scanner + firewall (Warden) | ✅ | ❌ | the firewall surface today |

### Direct answers to your specific questions
- **Browser / JS rendering:** `lens` is the "browser" but renders HTML as
  DISPLAYABLE TEXT (status line + body); there is **NO DOM parser and NO
  JavaScript engine**. The canonical cell-grid Surface is text-cell by design, so
  a *text/structured* renderer is the aligned target; a full JS engine is a large,
  separate capability NOT in the current vision docs (would be a major new axis —
  flagged below as a decision, not an assumed task).
- **Electron / web rendering for GUI & mobile:** NO. The canonical GUI/mobile are
  the **cell-grid `Surface`** (PLATFORMS.md), not a web stack. Electron/Chromium
  would CONTRADICT the from-scratch, cell-grid, no_std vision — that would be
  drift. UI is native Surface rendering; "superb UI/UX" is achieved WITHIN the
  cell-grid model (layout, color cells, gestures), and the hardware display driver
  blits the Surface. (If a richer pixel Surface is ever wanted, it is a Surface
  upgrade, still native — never an embedded browser engine.)
- **Settings app:** NONE exists. GAP (new finding) — a settings/control app is
  expected for a finalized OS (profile, storage, network/Warden, display).
- **Firewall app/feature:** YES — the **Warden** (per-Gate allow/deny + boundary
  ownership) is the firewall; `probe` is the firewall/scanner tool. No standalone
  "firewall app" with a polished UI yet (GAP — a Warden control UI).
- **Server (nginx replacement):** YES — `vane` is the content daemon. Runs on
  host today; not yet an on-kernel server-platform service.
- **Server orchestrator ("Proxmox-VE-for-CIBOS"):** NOT STARTED (no crate). This
  is Track 4, design-first (confirmed on-track earlier). A container/boundary
  orchestrator over the server platform — large, its own milestone.
- **Async runtime (custom, no Tokio):** DONE (`cibos-async-runtime`).

---

## 3. PER-PLATFORM / PER-ARCH COVERAGE MATRIX

### Architectures (the boot/runtime layer)
| Capability | x86_64 | aarch64 | riscv64 | i686 |
|---|---|---|---|---|
| Firmware (cibios) boots | ✅ | ✅ | ✅ | ✅ (build-std) |
| Kernel builds bare | ✅ | ✅ | ✅ | ✅ |
| MMU/paging + per-boundary spaces | ✅ | ⚠ partial | ⚠ partial | ⚠ needs MMU path |
| Ring-3 user mode + syscalls | ✅ | ❌ (x86-gated) | ❌ (x86-gated) | ❌ |
| Live ring-3 multi-context / spawn | ✅ | ❌ | ❌ | ❌ |
| Channels + cross-boundary handshake | ✅ | ❌ | ❌ | ❌ |
| Keyboard/input + login/shell | ✅ | ❌ | ❌ | ❌ |
| In-kernel cooperative executor | ✅ | ✅ | ✅ | ✅ |
GAP: the ring-3 app/login/IPC stack is **x86_64-only** today. aarch64/riscv64/
i686 build the kernel and run the in-kernel executor, but the ring-3 trap/entry,
syscall, and app flow are x86-gated. Per-arch ring-3 is a major finalization axis.

### Platforms (the four surfaces) — all host today; on-kernel pending
| Platform | Runner exists | Apps run on it (host) | On-kernel | Gap to finalize |
|---|---|---|---|---|
| CLI | ✅ | shell, all CLI apps | ✅ (shell/login on kernel) | port remaining CLI apps to `.capp` |
| GUI | ✅ | notepad, etc. (cell-grid) | ❌ | on-kernel Surface→framebuffer driver + GUI app loader |
| Mobile | ✅ | gesture apps | ❌ | on-kernel touch input + Surface; per-arch (ARM) |
| Server | ✅ | vane | ❌ | on-kernel server runner; orchestrator (Track 4) |

---

## 4. CAPABILITY GAPS + NEW FINDINGS (the finalization backlog)

### A. Networking (Track 3) — the immediate next, two faithful layers
1. Lattice on the kernel over syscalls (Gate/Link/Warden), loopback-backed —
   TRACK3-LATTICE-PLAN.md (reviewed, ready).
2. `NetDevice` trait + **virtio-net** driver behind it (real interface, QEMU-
   verifiable), then e1000 — the BlockDevice/ATA pattern.
3. A loopback→NIC transport switch beneath the SAME Gate/Link surface (apps
   unchanged — NETWORKING.md guarantee).

### B. Port the 14 host-only apps to on-kernel `.capp`s
Per platform: CLI apps first (smallest delta — shell already proves the path),
then GUI (needs the on-kernel Surface driver), then mobile (needs touch).
REUSE the verified `.capp` flow (login-rs/shell-rs pattern) — do NOT reimplement
the apps (they already exist and are tested); the on-kernel build is glue +
backends, exactly as shell-rs reuses `shell::dispatch`. Anti-redundancy rule.

### C. Per-arch ring-3 (aarch64 / riscv64 / i686)
Bring the ring-3 trap/entry + syscall + app/login flow to the other arches
(currently x86-gated). i686 also needs an MMU/paging path + a VGA path. Large but
well-scoped (the x86 implementation is the template).

### D. Missing apps (new findings — expected for a finalized OS)
- **Settings / control center** (profile, storage, network/Warden, display).
- **Warden firewall control UI** (the Warden exists; needs a usable surface).
- **File manager** (GUI; CIBOSFS browse — referenced in shell catalog, not built).
- **Terminal app** (GUI host for the shell on GUI/mobile).
- **System monitor** (lanes/boundaries/scheduler view — we have the data).
- **Network/Lattice browser** for Gates/Links (probe is CLI; a GUI version).
- Possibly: media/image viewer (within Surface limits), PDF/doc viewer (text).

### E. The server orchestrator — "Proxmox-VE-for-CIBOS" (Track 4)
Design-first (confirmed on-track). A boundary/container orchestrator over the
server platform: provision boundaries, attach storage volumes, bind Gates,
lifecycle (start/stop/migrate-design). Its own milestone after Track 3.

### F. Behavioral profile flags (deferred, acknowledged)
`cryptographic-ipc` (the additive crypto layer over the lightweight handshake we
built), anti-starvation, weight-aging, multi-user-isolation, audit-logging, etc.
— declared-but-inert; make profiles genuinely different binaries (ADR-007).

### G. The 8 canonical examples (deferred, acknowledged)
5 of 8 exist in `cibos-sdk/examples`; 3 missing (compute-intensive, event-driven-
ui, mobile-sensor). API conformance suite.

### H. UI/UX quality (cross-cutting requirement)
Within the cell-grid Surface model: consistent layout primitives, color/contrast,
focus/navigation, gesture affordances on mobile, a shared widget/skin layer so
all GUI apps feel coherent. NOT a web stack — native Surface UX. (Flagged as a
standing quality bar for every GUI/mobile app, per your "superb UI/UX always".)

---

## 5. ALIGNMENT WITH THE 18-DOC VISION (drift check)

- Boundary is the principal; isolation binary — UPHELD everywhere built.
- Single selector, no global locks across user exec — UPHELD.
- Channels: propose → accept-all-or-reject, point-to-point — UPHELD + tested.
- Lattice Gate/Link/Warden vocabulary — present (SDK); kernel port keeps the
  SAME surface (no new vocabulary) — the anti-drift rule for Track 3.
- Cell-grid Surface for GUI/mobile — UPHELD; Electron/JS-engine would be DRIFT
  and is explicitly NOT planned.
- From-scratch, no_std, real/no-fakes, honest hardware boundaries — UPHELD;
  virtio-net is a real interface (not a fake), consistent with BlockDevice/ATA.
- Four platforms + four arches — the COVERAGE MATRIX (§3) is the finalization
  scope; today's depth is x86_64 + host-runners, breadth is the remaining work.

NO DRIFT FOUND in what is built. The gaps are INCOMPLETENESS (breadth across
platforms/arches + new apps), not contradictions of the vision.

---

## 6. SUGGESTED FINALIZATION ORDER (faithful, dependency-aware)
1. Track 3A: Lattice on the kernel (Gate/Link/Warden over syscalls, loopback).
2. Track 3B: `NetDevice` trait + virtio-net driver (real, QEMU-verifiable).
3. Port CLI apps to on-kernel `.capp`s (reuse the shell-rs glue pattern).
4. On-kernel GUI Surface driver (Surface→framebuffer), then port GUI apps.
5. Per-arch ring-3 (aarch64 → riscv64 → i686).
6. Mobile on-kernel (touch input + Surface; ARM focus).
7. New apps: settings, file manager, terminal, Warden UI, system monitor.
8. Track 4: server orchestrator (design-first).
9. Behavioral flags + the 8 examples (the earlier-deferred items).
Each step: reuse verified flows (no redundancy), build-all + clippy + tests +
QEMU-verify, archive, keep docs true. UI/UX bar applies to every surface app.

---

## 7. LANGUAGES & CAPABILITIES — how JS (and others) could work in CIBOS

### The foundation (verified facts, not speculation)
Two things in the codebase make this answerable precisely:
1. **The syscall ABI is a STABLE NUMERIC ABI** (syscall number in `rax`, args in a
   fixed register convention; ABI version recorded). It does NOT assume Rust — it
   is a contract any native code can target. (shared/protocols/syscall.rs)
2. **The `.capp`/AppImage format is LANGUAGE-AGNOSTIC** — a generic loadable-
   segment format (vaddr + file_size + mem_size + RWX perms + entry vaddr), like
   ELF program headers. It carries machine code + data; it does not care what
   language emitted them. (shared/protocols/app_image.rs)

CONSEQUENCE: any toolchain that can (a) compile to native code for the target arch
and (b) emit the CIBOS syscall convention can produce a `.capp` that runs in ring-3
with ZERO kernel changes. Rust is the first such toolchain; it is not the only
possible one. We have Rust already — that is the native app language. The question
"do we port/fork/recreate for JS?" resolves into THREE distinct, honest options,
because JavaScript is not compiled-to-native the way Rust is — it needs a runtime.

### What "supporting JS" actually means (it is a RUNTIME, not just a language)
JavaScript needs a JS ENGINE (parser + interpreter/JIT + a garbage collector +
host bindings). So "allowing JS" is really "running a JS engine as a CIBOS app and
giving it CIBOS-native host bindings." Options, from least to most work:

- **Option A — Port an existing engine to `no_std` + the CIBOS syscall ABI.**
  Candidates that are designed to be embeddable / portable:
    * **QuickJS** (C, tiny, no OS assumptions beyond malloc + a few libc calls) —
      the most realistic first port. Needs: a `no_std` libc shim mapping malloc to
      the CIBOS allocator and file/console/clock to CIBOS syscalls. No JIT, so no
      executable-page generation needed (fits W^X / the AppImage perm model).
    * **Boa** or **Kogiri** (JS engines WRITTEN IN RUST) — most aligned: pure Rust,
      `no_std`-feasible, no C shim, compiles into a `.capp` the same way our Rust
      apps do. Strong candidate precisely because we are already a Rust shop.
    * V8 / SpiderMonkey — powerful but huge, JIT-heavy (needs W+X executable pages
      = a security/isolation tension with the binary-isolation model), heavy OS
      assumptions. NOT a good first fit; likely a non-goal.
  PORT (not fork) is the word: we wrap the engine, we do not change its semantics.
  A fork is only needed if an engine hard-codes OS assumptions we must replace.

- **Option B — Build our own minimal JS-subset engine in Rust.** Full control,
  `no_std` from day one, but JS is a large spec; a faithful engine is a multi-month
  effort and a maintenance burden. Only sensible if we want a deliberately small,
  auditable scripting surface rather than web-compatible JS.

- **Option C — Don't run JS; offer a CIBOS-native scripting language instead.**
  A small embedded scripting language (e.g. a Rust-hosted Lua-like or a CIBOS DSL)
  for app scripting/automation, sidestepping web-JS entirely. Aligned with the
  "from-scratch, auditable" vision; does NOT give web compatibility.

### RECOMMENDATION (aligned, no drift, decision deferred to you)
- For **app logic**: stay native. Rust `.capp`s are the first-class apps; other
  compiled languages (C/Zig/asm) can target the same ABI if ever wanted. No JS
  needed for apps themselves.
- For **scripting / "run a script"**: prefer **Option A with a Rust engine (Boa)**
  — it compiles into a `.capp` like everything else, needs no C shim, and keeps the
  whole stack Rust/auditable. This is the path that fits CIBOS best.
- For the **browser (`lens`)**: rendering web pages with JS is a SEPARATE, much
  larger goal than "run JS." A faithful CIBOS browser would be: Hail/HTTP fetch
  (have it) → an HTML parser → a layout engine over the (richer) Surface → optional
  JS via the embedded engine for scripted pages. This is a multi-stage roadmap
  item, NOT Electron/Chromium (which would be drift). Captured in the app list.
- **NEVER** embed Electron/Chromium/V8-as-the-platform: that contradicts the
  from-scratch, no_std, cell-grid/Surface, binary-isolation vision. The richer
  pixel Surface (now in scope) is the native answer for graphics, not a web engine.

### Richer pixel Surface (now in scope — confirmed)
Upgrade the `Surface` from character-cell to an optional PIXEL surface (a
framebuffer of RGBA pixels) while keeping the cell-grid as the simple tier. Apps
target a Surface trait; the hardware display driver blits either tier (the same
BlockDevice/NetDevice honest pattern). This unlocks: real fonts, images, a graphical
browser render target, charts, and a polished GUI/mobile UX — all NATIVE, no web
engine. This is a first-class finalization item (display driver + Surface v2).

### Other "capabilities to transfer over" (the norms a real OS provides)
Treat each as either (i) native Rust crate, (ii) ported library behind a CIBOS
shim, or (iii) explicit non-goal. First-pass classification:
- **Crypto/TLS** → native Rust (we already use Rust crypto); TLS via a Rust crate
  (rustls-like) ported `no_std`. Needed for a real network stack.
- **TCP/IP stack** → Rust (`smoltcp` is `no_std` and embeddable) behind the
  NetDevice/Lattice layer — the realistic path to real connectivity.
- **Fonts/text shaping** → Rust crate ported (for the pixel Surface).
- **Image decode (PNG/JPEG)** → Rust crates ported (for a viewer + browser images).
- **Compression (zlib/zstd)** → Rust crates ported.
- **Regex / JSON / parsing** → native Rust crates (mostly `no_std`-ready).
- **SQLite-class local DB** → port (C, via shim) OR a Rust embedded DB → app data.
- **POSIX/libc compatibility** → a SHIM only if we port C software; not a goal for
  native apps. Decide per ported component (e.g. QuickJS) — minimal shim, not a
  full libc.
The rule: prefer Rust-native or Rust-ported `no_std` libraries behind CIBOS traits;
use a C-shim port only when a needed component has no Rust equivalent; mark
anything that fights the isolation/no_std/from-scratch model as a non-goal.

---

## 8. FULL APP LIST TO CREATE — per platform (the complete target set)

Convention: ✅ exists (host), ⛓ exists on-kernel (.capp), ◻ to create. "Create"
means a NEW app or a NEW on-kernel port of an existing tested crate (REUSE the
crate; glue only — the shell-rs/login-rs rule; never reimplement).

### CLI platform (terminal) — target set
Core: ⛓ shell, ⛓ login, ✅ package-manager(pkg), ✅ trove(store), ✅ editor(edit),
✅ kvstore(kv), ✅ probe (net/firewall), ✅ calc-service.
To create / port: ◻ file-manager (CIBOSFS browse/copy/move/rm), ◻ terminal
multiplexer, ◻ text-viewer/pager, ◻ system-monitor (lanes/boundaries/scheduler),
◻ process/lane control (ps/kill-equiv over the lane model), ◻ disk/volume tool
(format/mount Live+Persistent), ◻ settings-cli (profile/network/display config),
◻ Warden-cli (firewall rules), ◻ net-tools (gate-probe/link-test/route once NIC),
◻ user/profile admin, ◻ log viewer (audit-logging flag), ◻ cron/scheduler-cli,
◻ archive tool (.capp/bundle pack-unpack), ◻ hex/inspect tool, ◻ env/config editor,
◻ help/man system, ◻ clipboard (cross-app via channel), ◻ search/grep tool,
◻ diff tool, ◻ checksum/verify tool, ◻ scripting runner (Option-A engine host).

### GUI platform (desktop, cell-grid → richer pixel Surface) — target set
Shell/Surface: ◻ desktop/launcher (app grid), ◻ window/pane manager (Surface
compositor), ◻ status bar, ◻ on-screen notifications, ◻ lockscreen (✅ crate →
port), ◻ login screen (⛓ logic → GUI surface).
Apps: ✅ notepad → ⛓ port, ◻ file-manager (GUI), ◻ terminal (GUI host for shell),
◻ settings/control-center (profile/display/network/storage/Warden), ◻ system-
monitor (graphical), ◻ text-editor (GUI, over editor crate), ◻ image-viewer
(needs pixel Surface + image decode), ◻ pdf/document-viewer (text→pixel),
◻ calculator (GUI, over calc-service), ✅ calendar → ◻ GUI, ✅ contacts → ◻ GUI,
✅ clock → ◻ GUI (+timers/alarms), ✅ courier(messaging) → ◻ GUI, ✅ postbox(email)
→ ◻ GUI, ◻ lens browser (GUI: Hail fetch → HTML parse → layout on pixel Surface →
optional JS engine), ◻ media/music player (audio is a later driver), ◻ photo
gallery, ◻ paint/draw (pixel Surface), ◻ screenshot tool, ◻ font/appearance
(themes for the Surface widget layer), ◻ app store (trove) GUI, ◻ package-manager
GUI, ◻ Warden/firewall GUI, ◻ network manager GUI, ◻ help/docs viewer,
◻ widget/skin library (shared UI kit — the "superb UX always" backbone).

### Mobile platform (touch, cell-grid → pixel Surface, ARM focus) — target set
Shell: ◻ home/launcher (tap grid), ◻ gesture nav (Tap/Swipe already modeled),
◻ status bar + notifications, ◻ lockscreen (touch), ◻ on-screen keyboard
(Surface input method — REQUIRED for touch text entry).
Apps (touch builds of the GUI set, reusing the same crates): ◻ phone/dialer-style
contacts, ◻ messaging (courier touch), ◻ email (postbox touch), ◻ calendar touch,
◻ clock/alarms touch, ◻ calculator touch, ◻ notes (notepad touch), ◻ files touch,
◻ settings touch, ◻ browser (lens touch), ◻ camera/gallery (needs sensor/driver),
◻ media player touch, ◻ app store (trove touch), ◻ maps (later), ◻ weather (needs
net), ◻ system/battery monitor, ◻ Warden/privacy controls touch.
Mobile-specific: ◻ sensor framework (the `mobile-sensor` example), ◻ power/battery
service, ◻ touch IME, ◻ haptics/notification service.

### Server platform (headless daemon) — target set
Services: ✅ vane (content daemon / nginx role) → ◻ on-kernel server service,
◻ reverse-proxy/load-balancer (Lattice Gate routing), ◻ Hail/HTTP server lib,
◻ TLS terminator (rustls-port), ◻ DNS-equiv resolver (over Lattice/NIC),
◻ DHCP-equiv (once NIC), ◻ storage/volume service, ◻ key-value/DB service
(kvstore → networked), ◻ log/audit aggregator, ◻ metrics/monitoring service,
◻ scheduler/cron daemon, ◻ backup/snapshot service, ◻ identity/auth service
(accounts → networked), ◻ container/boundary runtime (the orchestrator core).
**Orchestrator — "Proxmox-VE-for-CIBOS" (Track 4, design-first):** ◻ boundary/
container provisioning, ◻ volume management, ◻ Gate/network management, ◻ lifecycle
(start/stop/snapshot; migration = design), ◻ a control API (over Lattice), ◻ a web/
CLI admin surface (native; NOT a web-engine UI), ◻ cluster/multi-node (design).

### Cross-platform foundation libraries (not "apps" but required capabilities)
◻ Surface v2 (pixel framebuffer + cell-grid tiers) + display drivers (virtio-gpu/
VGA/framebuffer), ◻ NetDevice trait + virtio-net (+e1000) driver, ◻ TCP/IP stack
(smoltcp-port) under the Lattice, ◻ TLS (rustls-port), ◻ font/text-shaping, ◻ image
codecs, ◻ a shared UI widget/skin kit (consistent superb UX), ◻ an embedded
scripting engine (Boa-port) for Option-A, ◻ audio driver + service (later),
◻ USB/HID stack (later, for real keyboards/mice/touch on hardware), ◻ a libc shim
(only for ported C components like QuickJS, minimal).

### Per-arch note
Every app above first lands on **x86_64** (the ring-3 stack is x86-only today),
then comes to **aarch64 → riscv64 → i686** as per-arch ring-3 is finalized. The
app crates are arch-independent (they target the SDK/ABI); the per-arch work is in
the kernel ring-3 trap/entry/syscall path, not the apps.

---

## 9. PLAN FOR AFTERWARDS (updated finalization sequence, incl. languages)
1. **Track 3A** — Lattice on the kernel (Gate/Link/Warden over syscalls, loopback).
2. **Track 3B** — NetDevice trait + virtio-net driver (real, QEMU-verifiable),
   then smoltcp TCP/IP under the Lattice; e1000 as a second driver.
3. **Surface v2** — pixel framebuffer + display driver (virtio-gpu/VGA); keep the
   cell-grid tier. Unlocks real GUI/mobile UX + browser/image targets.
4. **Port CLI apps** to on-kernel `.capp`s (reuse crates; shell-rs glue pattern).
5. **On-kernel GUI** — Surface compositor/launcher + port GUI apps; shared widget
   kit for consistent, superb UX.
6. **Per-arch ring-3** — aarch64 → riscv64 → i686 (bring the x86 ring-3 stack over).
7. **Mobile** — touch input + IME + Surface; port the touch app set (ARM).
8. **New apps** — settings, file-manager, terminal, Warden UI, system-monitor,
   then the broader per-platform lists (§8), batched by shared dependencies.
9. **Scripting/JS** — port a Rust JS engine (Boa) as a `.capp` (Option A) for a
   scripting surface; later, the browser pipeline (HTML parse → layout → optional
   JS) on Surface v2. C-shim only if a needed component has no Rust equivalent.
10. **Track 4** — server orchestrator (Proxmox-VE-for-CIBOS), design-first.
11. **Deferred** — behavioral profile flags (incl. cryptographic-ipc) + the 8
    canonical examples.

Standing rules every step: reuse verified flows (no redundancy); native Surface UI
(never a web engine); honest hardware boundaries (real drivers for real interfaces,
flagged-not-faked); build-all + clippy + tests + QEMU-verify + archive; keep docs
true; superb UI/UX as a quality bar on every surface app.

---

## 7b. "Can we wrap it?" — the precise wrapping line (engine vs platform)

"Wrapping" at the OS level = giving a program a shim that maps the syscalls IT
expects onto the syscalls CIBOS provides. Whether something is wrappable depends
ENTIRELY on what OS surface it assumes.

- **A JS ENGINE is wrappable** (this is Option A, and the answer to "do we still
  wrap it?" = yes, wrap; not modify):
  * **QuickJS (C):** assumes only malloc/free + a few libc calls (file r/w, time).
    Wrap = minimal libc shim (malloc→CIBOS alloc, write→Log, file→Fs*, time→Now).
    Pure-JS (logic/JSON/compute) runs in a ring-3 `.capp`. JS semantics UNCHANGED —
    we satisfy the engine's modest OS expectations, we do not alter the language.
  * **Boa (Rust):** `no_std`-feasible; needs an allocator and little else. Almost
    NOTHING to wrap — it compiles into a `.capp` like our Rust apps. Preferred:
    all-Rust, auditable, least friction.
  * "Modify/fork" is only needed if an engine HARD-CODES an OS assumption we cannot
    satisfy. QuickJS/Boa do not, so we WRAP (or just compile), we do not fork.

- **ELECTRON is NOT wrappable** (and this is a hard architectural line, not effort):
  Electron is not an engine — it is Chromium (multi-process browser + GPU
  compositor + full HTML/CSS layout + sandbox) + Node.js (V8 JIT + libuv + a large
  POSIX surface) + a native windowing toolkit. It ASSUMES a POSIX/Linux (or Win/
  macOS) kernel: hundreds of syscalls, fork/exec, mmap with EXECUTABLE pages (V8
  JIT = W+X), epoll, shared memory, a GPU/display stack, full TCP/IP+TLS. To "wrap"
  it, CIBOS would have to BECOME a POSIX system — a Linux-compatible syscall layer,
  a fork/exec process model, a W+X memory model, a GPU/display server. That is not
  wrapping Electron onto CIBOS; it is REBUILDING LINUX UNDER IT, which directly
  contradicts from-scratch + binary-isolation + no_std + (no W+X). The V8 JIT's W+X
  requirement alone fights the isolation model at a deep level. => NON-GOAL / drift.

- **The faithful equivalent of "what Electron gives" (HTML/CSS app UIs)** is the
  NATIVE path: richer pixel Surface (v2) + an HTML/CSS layout renderer + optional
  Boa for page scripting. Same CAPABILITY (rich graphical UIs, web-style rendering)
  on an ALIGNED architecture (no foreign OS surface). This is the browser/Surface
  roadmap, not an embedded foreign runtime.

RULE OF THUMB: if a thing expects "an engine + an allocator + a few syscalls," we
wrap it. If it expects "an operating system" (process model, JIT/W+X, GPU server,
POSIX), we do NOT wrap it — we build the capability natively on the Surface. The
boundary is "engine vs platform," and it maps exactly onto the from-scratch vision.
