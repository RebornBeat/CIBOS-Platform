# AArch64 bring-up — diagnosis (B1 starting point)

## Honest current state
aarch64 builds (firmware + kernel core) but does NOT boot to a banner. This is a
pre-existing gap (the backend was 38 lines: PL011 putc + halt), not a regression.

## What the instruction trace shows (QEMU virt, -d in_asm)
_start (0x40080000) runs correctly: sets SP, clears BSS, calls kernel_entry.
kernel_entry -> init_serial (ok) -> the FIRST kprintln! -> into core::fmt
machinery (write_fmt -> as_statically_known_str -> from_raw_parts precondition ->
is_aligned_to) -> FAULTS at 0x400bfe88 -> jumps to 0x00000200.

0x200 is the synchronous-exception offset in an aarch64 vector table, but VBAR_EL1
is NOT set, so the CPU vectors to physical ~0x200 (garbage) = silent death. That
is why there is NO output at all: the fault happens DURING the first print, before
any byte is pushed to the UART, and there is no handler to report it.

## Two distinct problems, in order
1. NO EXCEPTION VECTORS (foundational): VBAR_EL1 is unset, so ANY fault on aarch64
   vanishes to a garbage low address with no report. This must be fixed FIRST so
   every subsequent fault is visible. (The x86 side has its IDT + fault reporter;
   aarch64 needs the equivalent: a 16-entry vector table at a 2KB-aligned base,
   VBAR_EL1 set to it, and a handler that reads ESR_EL1/ELR_EL1/FAR_EL1 and
   reports via the UART then halts.)
2. AN EARLY FAULT in the first kprintln's core::fmt path (an alignment / 
   from_raw_parts check). Likely SP or data alignment, or SCTLR_EL1.A. Once (1) is
   in place, this fault will be REPORTED (ESR_EL1 EC + FAR_EL1) and can be fixed
   precisely instead of guessed.

## Plan (no drift; mirrors the x86 fault path, aarch64 mechanisms)
B1a. Install an aarch64 exception vector table + VBAR_EL1 + a synchronous/IRQ/
     fault handler that decodes ESR_EL1 and reports via PL011, then halts. Verify
     it makes the existing early fault VISIBLE (we should now SEE an ESR dump
     instead of silence).
B1b. Fix the reported early fault (alignment/SP) so the banner prints and the
     arch-independent kernel core (scheduler + channel demo) runs on aarch64 —
     the same milestone x86 reached, proving the portable core on ARM.
B1c. Then the rest of B1 (timer via CNTP, GIC, MMU via TTBR/Sv-equiv, input),
     then per-arch drivers (virtio-mmio), then ring-3 (EL1->EL0), per the
     REMAINING-ARC plan. Each step QEMU-verified + archived.

## Note on serial
Output uses PL011 at 0x09000000 (QEMU virt). It works without init; the lack of
output is solely due to the fault-before-first-byte above, NOT a serial-routing
problem (confirmed: the trace never reaches putc).

---

## RESOLUTION — aarch64 now boots (B1a + B1b done)

Root cause (found via QEMU `-d in_asm,int`): the first fault was
`[Undefined Instruction] ESR EC 0x7` = "SVE/SIMD/FP disabled". At EL1, CPACR_EL1
traps Advanced SIMD/FP by default, and Rust's core library (core::fmt, memcpy)
uses SIMD registers — so the first such instruction trapped, and with no vectors
it vanished to 0x200.

Two fixes landed:
1. boot/aarch64.s: enable FP/SIMD before calling kernel_entry —
   `mrs x1, cpacr_el1; orr x1, x1, #(0b11 << 20); msr cpacr_el1, x1; isb`
   (CPACR_EL1.FPEN = 0b11, no trapping).
2. Exception vectors (vectors_aarch64.s): a 16-entry table at a 2KB-aligned base;
   install_exception_vectors() sets VBAR_EL1; cibos_aarch64_exception() decodes
   ESR_EL1 EC + ELR/FAR and reports via the PL011 console, then halts. So any
   future aarch64 fault is REPORTED, not silent (the x86-IDT equivalent).

VERIFIED (QEMU virt, -kernel, self-boot):
    CIBOS kernel: entry
    CIBOS kernel: heap online (8388608 bytes)
    CIBOS kernel: handoff accepted, 134217728 bytes usable across 1 region(s)
    CIBOS kernel: init lane running
    CIBOS kernel: scheduler idle after 1 poll(s)
    CIBOS kernel: boot complete
The arch-independent kernel core (heap, handoff, scheduler, init lane) runs on
ARM. x86 unaffected; 370 tests green; aarch64 builds clean (0 warnings).

## RISC-V 64 — next (same approach)
riscv64 currently boots SILENT (no output) — the same class of early-death.
Next increment (B1 for riscv64): set the trap vector (stvec) + a trap handler
that reports scause/sepc/stval; enable FP if used (sstatus.FS); confirm the SBI
console path prints; then the kernel core should boot as on aarch64. Mirror the
aarch64 sequence: vectors/trap-report FIRST (make faults visible), then bisect.

## Remaining for full per-arch (unchanged, per REMAINING-ARC plan)
Per arch after the core boots: timer (aarch64 CNTP / riscv64 SBI-timer), interrupt
controller (GIC / PLIC), MMU + identity map (TTBR / Sv39), input, ring-3
(EL1->EL0 / S->U), then per-arch virtio-mmio drivers, then platforms + the
verification matrix.
