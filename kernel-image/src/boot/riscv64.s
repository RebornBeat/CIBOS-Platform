// CIBOS kernel RISC-V 64 boot entry (QEMU `virt`, S-mode).
//
// Entered (per the RISC-V/OpenSBI boot protocol) with the hart id in a0 and the
// DTB pointer in a1. The CIBIOS path instead passes the handoff pointer in a0.
// We preserve BOTH a0 and a1 across BSS-clear so kernel_entry receives
// (a0, a1) = (handoff-or-hartid, dtb-or-zero); the Rust side decides which boot
// path it is. We set up the stack, clear BSS, and call kernel_entry(a0, a1).
.section .text.boot
.global _start
_start:
    // Preserve the two boot arguments across BSS-clear (s0/s1 are callee-saved
    // but we are the entry; use t-regs we restore right before the call).
    mv   s0, a0
    mv   s1, a1

    // Set up the stack.
    la   sp, __stack_top

    // Clear BSS.
    la   t0, __bss_start
    la   t1, __bss_end
1:  bgeu t0, t1, 2f
    sd   zero, 0(t0)
    addi t0, t0, 8
    j    1b
2:
    // Restore the boot arguments into a0/a1 for kernel_entry(a0, a1).
    mv   a0, s0
    mv   a1, s1
    call kernel_entry
3:  wfi
    j    3b
