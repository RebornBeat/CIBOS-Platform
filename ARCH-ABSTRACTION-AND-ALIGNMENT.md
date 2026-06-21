# Arch abstraction, x86_64 alignment, and no-drift guarantees

Answering the standing questions: as we close per-arch gaps, are we keeping
x86_64 aligned to the original findings, is the code clean to switch arches via
flags (no redundancy), and does the legacy x86 (i686) build fully?

## 1. Honest per-arch state (verified)
| Arch    | Backend                                   | State |
|---------|-------------------------------------------|-------|
| x86_64  | x86_64.rs (244L) + gdt/idt/paging/vga/ata/virtio_net/e1000/ring3_ctx | FULL — the only complete arch |
| aarch64 | aarch64.rs (serial + exception vectors + FP enable) | core BOOTS (scheduler/init-lane to "boot complete") |
| riscv64 | riscv64.rs (34L: SBI console + halt)      | silent; not yet booting (next) |
| i686    | x86.rs (COM1 serial + halt)               | builds; serial-only stub, NOT the full x86_64 stack |

IMPORTANT: i686 ("x86 legacy") does NOT fully build the stack — it is a
serial+halt bring-up stub at the SAME level as aarch64/riscv64, NOT a 32-bit
clone of the x86_64 backend. The earlier "x86_64 + i686 produce bootable .img"
refers to the FIRMWARE/boot path, not a full i686 kernel runtime. Full i686 =
its own bring-up effort (32-bit IDT, paging, reuse PCI/ATA/VGA/virtio drivers).

So: we have been working on x86_64 almost exclusively for the full stack; the
other three (incl. i686) are bring-up stubs at varying completeness.

## 2. The no-drift guarantee — why x86_64 stays aligned as others grow
The canonical invariants live in `cibos-kernel`, which has ZERO `target_arch`
references (verified). The scheduler, single selector, Catch-and-Release,
channels, gates/Lattice, boundaries, FS, and the cibos-net stack are ALL
arch-independent Rust. Consequences:
  - Every arch runs the IDENTICAL canonical core. aarch64 booted and ran the
    scheduler + init lane with NO core changes — proof the core is portable.
  - Bringing up a new arch CANNOT silently alter x86_64 behavior, because the
    shared code is arch-free and the per-arch code is isolated behind `arch::`.
  - The HIP invariants are therefore enforced uniformly across all arches by
    construction, not by per-arch re-implementation. This is the structural
    defense against drift.

## 3. The arch contract (clean switching via cfg, minimal redundancy)
`kernel-image/src/arch/mod.rs` exposes ONE surface; each arch backend implements
it behind `#[cfg(target_arch = ...)]`:
  - Common to all: `halt`, `init_serial`, `putc`.
  - x86_64 additionally: the full set (GDT/IDT/PIC/PIT/paging/ports/vga/...).
The other arches GROW their backend toward the x86_64 surface as they reach
parity. Per-arch branching in `kernel-image` is concentrated in boot.rs (the
bring-up sequence) and isolated — the core never branches.

Current boot.rs: 28 of 33 cfgs are x86_64-gated (the only full arch); the x86
bring-up (MMU, NIC, ring-3) is a COHESIVE gated block + gated helpers, not
scattered. This is acceptable WHILE x86_64 is the sole complete arch.

## 4. The refactor guardrail (when, not now)
A per-arch "BringUp" contract (each arch implements named phases: vectors -> MMU
-> timer -> intr-controller -> input -> ring-3) is the right end state to keep
boot.rs from accreting cfg blocks as arches grow. BUT designing it now, against a
sample size of ONE complete arch, is premature abstraction. Plan: bring riscv64
up to a booting core the same disciplined way aarch64 was done; once TWO arches
need the same bring-up phases, EXTRACT the common shape into a contract (the
pattern will be real, not guessed). Until then, keep the clean `arch::` module
seam and isolated gating. This avoids both drift AND speculative over-engineering.

## 5. Bare-metal-first reminder (holds per arch)
Every arch backend targets STANDARD hardware interfaces, unaware of QEMU:
x86 (0xB8000 VGA, 0x3F8 COM1, 8259 PIC, PCI 0xCF8), aarch64 (PL011 @ 0x09000000,
VBAR_EL1, CPACR_EL1), riscv64 (SBI console, stvec). QEMU presents these standard
interfaces so it VERIFIES the code; we never build "for QEMU". A device-tree was
used only to CONFIRM the PL011 address is the standard one — not to special-case
QEMU.

---

## UPDATE — three arches now boot the core (verified)

| Arch    | Boots core? | Fault visibility            | Notes |
|---------|-------------|-----------------------------|-------|
| x86_64  | YES (full)  | IDT + fault reporter        | full stack: ring-3, drivers, net |
| aarch64 | YES (core)  | VBAR_EL1 vectors + ESR dump | FP/SIMD enabled; scheduler+init lane run |
| riscv64 | YES (core)  | stvec trap + scause dump    | SBI console; scheduler+init lane run |
| i686    | builds      | (32-bit IDT pending)        | boots via CIBIOS firmware, not -kernel; runtime parity is a later increment |

aarch64 + riscv64 each now: (1) enable/operate FP as needed, (2) install a
fault/trap reporter (so future faults are VISIBLE, not silent — the x86-IDT
equivalent), and (3) run the arch-independent kernel core (heap, handoff,
scheduler, init lane) to "boot complete". This proves the canonical core is
portable across all three with NO core changes — the no-drift guarantee in action.

riscv64 was NOT broken earlier — it needed OpenSBI present (default -bios); the
prior "silent" run used `-bios none` (a test-harness error, not a kernel bug).
Verified the fact rather than "fixing" a non-bug.

NEXT per arch (same disciplined order, now that faults are visible on each):
timer -> interrupt controller -> MMU + identity map -> input -> ring-3 ->
per-arch virtio-mmio drivers -> platforms -> the verification matrix. The common
bring-up shape across aarch64+riscv64 is now visible enough to consider extracting
a per-arch BringUp contract (the refactor guardrail in section 4) when the timer/
MMU phases land on the second arch.
