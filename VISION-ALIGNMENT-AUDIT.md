# CIBOS / CIBIOS / HIP — vision-alignment audit

Review-only (no code edited). Purpose: re-read the project's OWN vision and
supporting docs (README, ARCHITECTURES, PROGRESS-AND-ROADMAP, NETWORKING,
PLATFORMS, SECURITY-NOTES, BOOT) and confirm that (a) every edit made so far
aligns with the stated vision, and (b) the planned future edits do too — and to
flag anything that has drifted.

## 1. THE VISION (from README.md, in the project's own words)

* **CIBIOS** = firmware (replaces BIOS/UEFI); **CIBOS** = microkernel OS; **HIP**
  = Hybrid Isolation Paradigm.
* **HIP's defining invariant:** "the security principal is the *boundary*, not a
  user account, and isolation is binary — **maximal or none, never tiered**."
* **Privacy-focused**, from-scratch Rust, single workspace.
* **Four architectures**, explicitly including **32-bit x86 for old hardware**
  ("meant to run on any device, however old").
* **Discipline (stated as a project value):** "Everything is real, compiles, no
  placeholders or mocks. Where a capability depends on hardware, that boundary is
  called out **honestly** in the docs rather than faked."
* Four platforms (CLI/GUI cell-grid/mobile touch/server daemon), the **Lattice**
  networking vocabulary, boundary isolation + human auth gating a profile.

## 2. ALIGNMENT OF WORK DONE — verified against the docs

| Work delivered | Vision/roadmap anchor | Verdict |
|---|---|---|
| Port apps onto `cibos-app` (`.capp`s on the kernel shell) | README: "porting them onto cibos-app … is in progress" | ALIGNED |
| Persistent CIBOSFS (mount-or-format) + local repo install | README storage: "CIBOSFS is the on-disk backing the Persistent volume is being wired onto" | ALIGNED |
| Security: firmware verify dispatches on algorithm; fail-closed | SECURITY-NOTES "in-firmware verification RESOLVED"; roadmap #1 | ALIGNED (and roadmap #1 is DONE) |
| platform-gui/mobile → no_std (kernel-ready) | PLATFORMS: cell-grid Surface, four platforms | ALIGNED |
| i686 kernel builds + BOOTS (qemu-system-i386) | ARCHITECTURES: X86=3, "32-bit x86 for old hardware" | ALIGNED (closes a named arch) |
| Display driver: Surface → VGA text blit (notepad on screen) | README/PLATFORMS: GUI is a **cell-grid** Surface; roadmap 2.1 #7 input/#8 surface | ALIGNED (cell-grid → VGA text is the intended target) |
| F1: login gates the shell session | NETWORKING "Isolation and accounts": human auth "gates entry to a profile; orthogonal to boundary isolation" | ALIGNED — login is a profile-entry gate, NOT a per-user isolation principal; the boundary remains the principal |
| `Sleep` syscall (ring-3 → kernel timer) | roadmap 2.1 #3(d)/#4: "grow the ABI to marshal the rest of the SDK System surface (channels, spawn, **sleep**)" | ALIGNED — SDK `System::sleep` exists; we are growing the ABI toward the documented surface |
| `ShellFs::list` single contract + `all_keys` | internal correctness (no vision conflict) | ALIGNED |

Discipline check (the cardinal rule): scanned every file I added/changed for
placeholders/mocks/fakes/TODOs in shipping code — NONE (the only "Mock" is a
`#[cfg(test)]` test double, which is normal). The `Sleep` impl is an honest
timer-backed wait (PIT `now_millis` + `sti;hlt`), not a no-op pretending to
sleep. Test count has GROWN 298 → 319 (added tested capability, no regression).

CONCLUSION (work done): every edit maps onto the project's own README + the
PROGRESS-AND-ROADMAP critical path (2.1 #2 MMU/isolation, #3 app loader, #4
syscall breadth, #7 input, #8 shell/login). No invariant was violated. The HIP
"boundary, not user; binary isolation" premise is intact — the login gate is
explicitly the orthogonal profile-entry auth the docs describe.

## 3. ALIGNMENT OF PLANNED WORK — with ONE drift flag

The remaining tracks map onto the roadmap as follows:

* TRACK 2 multi-context + `OpenChannel`/`Spawn` → roadmap 2.1 #3(c) preemptive
  multitasking + #3(d) ABI breadth. ALIGNED.
* TRACK 3 network/Lattice over syscalls → NETWORKING.md roadmap (Vane → Lens →
  Hail → Gate-by-boundary → NIC). ALIGNED — and note the README already names
  Vane/Lens/Hail/Warden/Probe, so this is finishing documented work.
* Per-arch finish (run app/login flow on aarch64/riscv64/i686; aarch64
  PageTableEncoder; i686 MMU + VGA) → roadmap 2.1 #2 ("add an aarch64
  PageTableEncoder"), Part 3 #7 breadth (i686, ARM images). ALIGNED.
* F1 live interactive session (real keyboard loop) → roadmap 2.1 #8. ALIGNED.
* Behavioral profile flags (anti-starvation, weight-aging, cryptographic-ipc,
  multi-user-isolation, audit-logging, …) → roadmap 2.1 #5. ALIGNED — NOTE this
  is the substance behind "profiles genuinely differ" (ADR-007) and we have NOT
  yet touched it; it should be on the near list.
* The 8 documented examples (hello-lane, channel-communication, …) → roadmap 2.1
  #6 / Part 3 #4. ALIGNED — NOTE these are the canonical API-conformance suite
  and we have NOT built them; per the project's order they rank BEFORE breadth.

>> DRIFT FLAG — Server orchestrator ("Proxmox-VE-for-CIBOS"):
   This appears ONLY in my forward-plan docs. It is NOT in the README or
   PROGRESS-AND-ROADMAP. The README describes the server platform as a "headless
   daemon," and roadmap 2.8 (scope-creep triage) explicitly says there are
   already 17 apps vs 8 documented examples and to "keep what maps to a real
   need, … shelve the rest — deliberately, on your call." So the orchestrator is
   NET-NEW SCOPE I proposed, not part of the documented vision. RECOMMENDATION:
   treat it as PROPOSED, pending Christian's explicit go — do not implement it as
   a committed track until then. (It may well be desirable; it just isn't in the
   vision yet, and the project's own rule is that such additions are a deliberate
   owner decision.)

## 4. DOC-SYNC ITEMS (small, non-behavioral — to fix when editing resumes)

* README says "298 unit tests" — now 319. Update the count (and the boot-chain /
  i686 / display lines) to reflect what now boots and runs.
* README storage line ("Host model today; CIBOSFS … being wired onto") — the
  Persistent volume now mounts real CIBOSFS across reboots; update.
* README applications line ("porting … in progress") — 5 apps now run on the
  kernel shell; notepad renders on VGA; update the status.
* Keep PORT-PLAN-AND-REVIEW.md and FORWARD-PLAN-…md as the working trackers, but
  fold their confirmed-done items back into PROGRESS-AND-ROADMAP.md so the
  project's canonical roadmap stays the single source of truth (avoid parallel
  roadmaps drifting).

## 5. RECOMMENDATION FOR NEXT ORDER (re-anchored to the PROJECT's order)

The project's Part 3 order, with our completions marked, suggests the truest-to-
vision continuation is:

1. (#1 no_std SPHINCS+) — DONE.
2. (#2 MMU/isolation) — core DONE; FINISH: aarch64 PageTableEncoder + wire
   AddressSpaceManager into the Kernel struct (per-arch isolation parity).
3. (#3 app loader/#4 syscalls) — running; CONTINUE Track 2 multi-context +
   channels/spawn (the ABI breadth the roadmap names).
4. (#4 examples) — BUILD the 8 documented examples (API-conformance; not yet
   done; ranks before breadth in the project's order).
5. (#5 behavioral flags) — make the profiles genuinely differ (not yet touched).
6. (#6 input+shell) — display DONE; FINISH the live interactive session (F1).
7. (#7 breadth) — per-arch app flow, i686 MMU/VGA, NIC, then network/Lattice.
   Server orchestrator ONLY if approved (see drift flag).

Net: we are well-aligned and on the critical path. The two things we'd been
under-weighting relative to the project's OWN order are the **8 documented
examples** and the **behavioral profile flags** — both are conformance/substance
items the roadmap ranks ahead of breadth. The one thing to NOT do without a
deliberate call is the server orchestrator.
