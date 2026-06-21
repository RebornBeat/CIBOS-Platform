# Per-arch BringUp contract — extracted from the x86_64 reference

## The correction (why now, not "later")
Earlier reasoning said: wait for a second arch before extracting a bring-up
abstraction (sample size of one). That was BACKWARDS. x86_64 is not "one sample" —
it is the REFERENCE IMPLEMENTATION of the canonical HIP bring-up. Letting each
arch grow its own shape and converging later GUARANTEES drift + redundant work to
reconcile. The aligned approach: extract the contract from x86_64 NOW and make
every other arch implement that SAME contract, so all four are aligned by
construction from the first line. One pattern, per arch, reviewed as one.

## The phases (extracted from the actual x86_64 kernel_entry sequence)
The canonical bring-up, in order, as x86_64 already performs it:
  1. early_traps      — make faults VISIBLE before anything can fault
                        (x86_64: IDT later but PIC/serial here; aarch64: VBAR_EL1
                        vectors + FP enable; riscv64: stvec). Already per-arch.
  2. heap             — portable (init_heap); NOT arch-specific. Stays in kernel_entry.
  3. handoff          — portable (obtain_handoff + profile check). Stays.
  4. scheduler_core   — portable (spawn init lane, run_until_idle). Stays.
  5. seed_entropy     — CSPRNG from handoff seed. Arch-INDEPENDENT logic but
                        currently x86-gated; should run on all (the RNG is portable).
  6. mount_root_fs    — block storage (ATA today). Per-arch driver.
  7. bring_up_mmu     — build+install page tables + identity map. Per-arch encoder.
  8. verify_storage   — read-back proof. Per-arch driver.
  9. probe_nic        — NIC discovery + install. Per-arch driver.
 10. start_ring3      — GDT/IDT + drop to user + syscalls. Per-arch.

Phases 2,3,4 are PORTABLE (pure cibos-kernel) — they already run on every arch
and stay directly in kernel_entry. Phases 1,5,6,7,8,9,10 are the PER-ARCH contract.

## The contract
A trait `ArchBringUp` (in kernel-image, implemented per arch) with one method per
per-arch phase, each returning a `PhaseStatus { Done, Skipped(reason), Failed(e) }`
so an arch that hasn't built a phase yet reports "Skipped(pending)" HONESTLY
instead of the phase being absent/cfg-gated. kernel_entry calls the phases in the
canonical order against `arch::BRINGUP` — identical control flow on every arch.

  trait ArchBringUp {
      fn early_traps(&self);                         // phase 1
      fn seed_entropy(&self, seed: &[u8]);           // phase 5 (portable body, per-arch hook)
      fn mount_root_fs(&self) -> PhaseStatus;        // phase 6
      fn bring_up_mmu(&self, h: &HandoffData) -> PhaseStatus; // phase 7
      fn verify_storage(&self) -> PhaseStatus;       // phase 8
      fn probe_nic(&self, frames: &FrameAllocator) -> PhaseStatus; // phase 9
      fn start_ring3(&self, ...) -> PhaseStatus;     // phase 10
  }

x86_64's impl wires the EXISTING functions (bring_up_mmu, verify_storage,
probe_nic_at_boot, start_ring3_runtime) — no behavior change, just relocation
behind the trait. aarch64/riscv64 impls return Done for early_traps (they have
vectors) and Skipped("pending: <phase>") for the rest, UNTIL each is built — at
which point the SAME method is filled in, never a new cfg block.

## Why this is the no-drift design
- kernel_entry becomes arch-agnostic: one ordered list of phase calls, zero
  target_arch cfgs in the control flow.
- Every arch is FORCED onto the identical phase sequence — divergence is
  impossible because the trait defines the shape.
- Building a new arch = filling in Skipped phases one by one, each verified, with
  the x86_64 impl as the executable spec of what that phase must achieve.
- Review-as-one: all four arch impls sit side by side implementing the same
  trait, so alignment is auditable at a glance.

## Migration (no behavior change to x86_64)
1. Define ArchBringUp + PhaseStatus.
2. x86_64 impl delegates to the existing functions (verify x86 boot UNCHANGED).
3. aarch64/riscv64 impls: early_traps Done, rest Skipped(pending).
4. Rewrite kernel_entry to call phases via arch::BRINGUP in canonical order.
5. Verify all three arches boot exactly as before (x86 full, arm/riscv core).
Then future per-arch work = implementing Skipped phases against the trait.

---

## MIGRATION COMPLETE — verified

The contract is implemented and the migration landed with NO behavior change to
x86_64. Files:
  - kernel-image/src/bringup.rs — the ArchBringUp trait + PhaseStatus.
  - kernel-image/src/boot.rs — four side-by-side `impl ArchBringUp for Arch`
    blocks (x86_64 / aarch64 / riscv64 / x86), each implementing the SAME phases.
    x86_64 delegates to the existing verified functions (seed_kernel_rng,
    mount_root_fs_early, bring_up_mmu, verify_storage). The others return
    Skipped("pending: <phase>") for unbuilt phases.
  - kernel_entry now calls Arch.early_traps / seed_entropy / mount_root_fs /
    bring_up_mmu / verify_storage in canonical order — ZERO target_arch cfgs in
    the boot control flow (was 5+ scattered). The redundant non-x86 bring_up_mmu
    stub was deleted.

VERIFIED:
  - x86_64: full stack UNCHANGED — MMU online, container isolation, DNS STACK OK,
    REMOTE LINK OK, boot complete. No "skipped" lines (it does every phase).
  - aarch64: boots core; reports "entropy seed skipped (pending: aarch64 RNG
    path)" and "MMU bring-up skipped (pending: aarch64 MMU encoder (TTBR/4KB
    granule))" — the roadmap is now visible in the boot log itself.
  - riscv64: boots core; reports the riscv64 pending phases (RNG, Sv39 MMU).
  - 370 tests pass; all x86 configs + all 4 arches build clean; i686 builds.

## RESULT — the alignment win
Every arch now follows the IDENTICAL bring-up sequence by construction. Building
an arch forward = replacing a Skipped phase with a real impl of the SAME method,
with x86_64's impl as the executable spec. The four impls sit side by side and
are auditable as one. No divergence is possible because the trait defines the
shape. This is the "sample size of one is the point" approach: x86_64 is the
reference, and every other arch is built ON its phases, not separately.

## NEXT (per arch, against the contract)
Implement the Skipped phases, x86_64 impl as the spec:
  aarch64: bring_up_mmu (TTBR0/1 + 4KB granule encoder + identity map), then
    mount_root_fs + verify_storage (virtio-mmio block), then NIC (virtio-mmio)
    inside the MMU phase scope, then ring-3 (EL1->EL0). riscv64: same with Sv39 +
    PLIC + S->U. Each phase: implement the trait method, QEMU-verify, archive.
The contract also makes the NEXT abstraction obvious if needed: when the MMU
phase's sub-structure (frames/space ownership + NIC + ring-3) recurs on a second
arch, factor that shared scope too — but only then.
