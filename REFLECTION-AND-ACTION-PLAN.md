# Reflection on the Cleanup + Verified Action Plan (checked against the 18 docs)

## PART 1 — Did the production cleanup serve the original 18-doc vision? (YES)

The canonical vision states (README): "Everything here is real, compiles... There
are no placeholders or mocks. Where a capability genuinely depends on hardware,
that boundary is called out honestly in the docs rather than faked." And the docs
repeatedly frame what is real as the "production default" (SECURITY-NOTES,
NETWORKING). The cleanup advanced exactly this principle. Action by action:

| Cleanup action | 18-doc principle it served | Verdict |
|---|---|---|
| `run_ring3_demo` → `start_ring3_runtime` (+ prod doc) | "real, production" — the ring-3 bring-up IS production; calling it a "demo" understated reality and invited treating it as throwaway | ALIGNED — honest naming |
| `demonstrate_keyboard_input` → `arm_keyboard_input` | same — IRQ1 input arming is production | ALIGNED |
| `demonstrate_storage` → `verify_storage`; `demonstrate_container_isolation` → `verify_container_isolation` | production read-only boot checks (MBR/BLD/isolation) are real verification, not demos; the destructive write round-trip stays `storage-selftest` | ALIGNED — production check vs opt-in selftest cleanly split |
| PROMOTE channel + Lattice table install into `start_ring3_runtime` (un-gate from `ring3-multilane-demo`) | the IPC + Lattice syscalls are production capability (HIP channels; NETWORKING Gate/Link/Warden). Trapping them inside a demo meant a normal boot couldn't use them — the capability was real but unreachable in production. Now reachable. | ALIGNED — removed a real drift |
| `gui-demo` driven by LIVE keyboard, not injected text | "called out honestly rather than faked" — injected input is a test double; the production display path reads the real keyboard | ALIGNED — removed a fake from the demo's prod-facing path |
| inject_text/enter/key confined to `all(storage-selftest, app-login)` + de-QEMU'd docs | selftest scaffolding is honestly labeled and isolated; production interactive surface is `interactive-session` (live) | ALIGNED |
| virtio-net driver promoted to always-compiled + boot-probed (prior step) | "real driver for a real interface; hardware boundary called out honestly" (NETWORKING: NIC layer beneath the same Lattice surface) | ALIGNED |

Confirmed-already-correct (the cleanup did NOT need to touch, and must not):
- DEFAULT build is bare-metal-first: reads the REAL CIBIOS handoff
  (`core::ptr::read(ptr)`); only `self-boot` synthesizes one for `-kernel`. This
  is the BOOT.md/SECURITY-NOTES production posture. Untouched.
- The three remaining `demonstrate_*` fns (fs_syscalls, kernel_channel, channel)
  are CORRECTLY gated to `storage-selftest`/`channel-demo` — genuine demos with
  honest names. No mis-naming remains.

### Did we drift from any invariant? NO.
- Binary boundary isolation, single selector, no global locks, channel mutual-
  agreement, cell-grid Surface, custom async runtime: untouched by the cleanup
  (it was naming + gating + reachability, not semantics).
- "No fakes / honest hardware boundary": the cleanup STRENGTHENED this — it
  removed an injected-input fake from a prod-facing path and exposed real
  capabilities (IPC/Lattice) that were hidden behind a demo flag.
- The QEMU posture now matches the canon: QEMU is the VERIFIER of production code
  (BOOT.md / QEMU.md), never the thing we build FOR. `self-boot` is the only
  QEMU affordance and is opt-in.

Net: the cleanup was corrective, not divergent. It made the codebase MORE faithful
to "real, production, honest" by deleting QEMU-orientation and demo-trapping.

---

## PART 2 — What's left, checked against the 18 docs (the action plan)

Each item is classified: does it ADVANCE a canonical goal, and is the APPROACH
faithful (no fakes, honest hardware boundary, reuse verified flows, native
Surface, single-selector/boundary invariants)?

### Immediate cleanup tail (finish the production posture)
A. [PROMOTE] GUI display driver (Surface→VGA, `crate::gui`): make it always-
   compiled production code with a production GUI boot path; `gui-demo` becomes an
   app-selection only.
   - Canon: PLATFORMS.md (GUI = cell-grid Surface; "a hardware display driver
     renders the same Surface to a framebuffer"). The driver is production; gating
     it behind a demo is the same drift we just fixed for virtio-net/IPC.
   - Approach check: ALIGNED — mirrors the BlockDevice/ATA + NetDevice/virtio
     pattern (portable Surface + concrete display driver). No fake.
B. [VERIFY] Confirm the production interactive image runs login→shell on the LIVE
   keyboard as the default posture (interactive-session), injected path = selftest
   only. (Mostly done; this is a verification + doc-truth pass.)

### Track 3B — real networking (the hardware layer the canon explicitly defers)
C. virtio-net TX/RX over the virtqueues (frame send/receive).
   - Canon: NETWORKING.md item 5 "A NIC-backed transport beneath the Lattice."
   - Approach: real virtqueue DMA; honest until verified. ALIGNED.
D. e1000 driver — a 2nd NetDevice impl so bare-metal boxes without virtio have a
   NIC.
   - Canon: same; the trait exists precisely for multiple real drivers.
   - Approach: real driver for a real interface (BlockDevice/ATA pattern). ALIGNED.
E. Wire the chosen NIC under the Lattice's NIC-backed transport — beneath the SAME
   Gate/Link/Warden surface; apps unchanged.
   - Canon: NETWORKING.md "applications will not change when that layer is added;
     only the fabric's backing transport does." ALIGNED — this is the exact
     guarantee. A TCP/IP stack (smoltcp-port, no_std) sits here.

### Breadth across platforms/arches (the finalization matrix)
F. Surface v2 (pixel framebuffer + the cell-grid tier) + display driver.
   - Canon: PLATFORMS Surface model; user-approved richer pixel Surface. Native,
     NOT a web engine (Electron = drift, already ruled out). ALIGNED.
G. Port the host-only apps to on-kernel `.capp`s (CLI first, then GUI, then
   mobile), REUSING the tested app crates (the shell-rs/login-rs glue pattern — no
   reimplementation, the anti-redundancy rule). ALIGNED.
H. Per-arch ring-3 (aarch64 → riscv64 → i686): bring the x86-only ring-3 trap/
   entry/syscall/app flow to the other arches.
   - Canon: four-arch support is a core goal; today's ring-3 stack is x86-only
     (honest gap). ALIGNED.
I. New apps for a finished OS: settings, file-manager, terminal, Warden/firewall
   UI, system-monitor; then the broader per-platform lists in
   FINALIZATION-INVENTORY §8. ALIGNED (fill real gaps; native Surface UX).

### Larger goals (design-first, as the canon frames them)
J. Server orchestrator ("Proxmox-VE-for-CIBOS"): boundary/container provisioning,
   volumes, Gate/network mgmt, lifecycle — Track 4, design-first. ALIGNED.
K. Scripting/JS: port a Rust JS engine (Boa) as a `.capp` (wrap, not modify); a
   C-shim port (QuickJS) only if needed. NEVER embed Electron/Chromium (would
   require reimplementing a POSIX OS = drift). ALIGNED with the languages analysis.
L. Behavioral profile flags (cryptographic-ipc as the additive layer over the
   lightweight handshake, anti-starvation, weight-aging, audit-logging) + the 8
   canonical examples. Deferred-but-acknowledged; ADR-007. ALIGNED.

### Suggested order (dependency-aware, unchanged in spirit)
1. Finish cleanup tail: A (GUI driver promote), B (verify live interactive default).
2. Track 3B: C (virtio TX/RX) → D (e1000) → E (NIC under Lattice + smoltcp).
3. Surface v2 (F) → port CLI apps (G) → on-kernel GUI + apps.
4. Per-arch ring-3 (H) → mobile.
5. New apps (I) → Track 4 orchestrator (J).
6. Scripting/Boa (K) → behavioral flags + examples (L).
Standing rules every step (all from the 18 docs): real/no-fakes, honest hardware
boundary, reuse verified flows (no redundancy), native Surface (never a web
engine), preserve binary-isolation/single-selector/no-global-lock invariants,
build-all + clippy + tests + boot-verify + archive, keep docs true, superb native
UI/UX as a quality bar.

## PART 3 — Verdict
The cleanup was faithful and corrective. Everything remaining maps to a stated
18-doc goal with a faithful approach; nothing on the list requires drifting from
the vision (the one perennial temptation — Electron/JS-as-platform — is explicitly
ruled out and replaced with the aligned native path). Proceed in the order above.
