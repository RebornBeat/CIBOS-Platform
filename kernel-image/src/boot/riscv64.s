// CIBOS kernel RISC-V 64 boot entry (QEMU `virt`, S-mode).
//
// Entered with the handoff pointer in a0 (CIBIOS path) or the hart id (QEMU
// self-boot via OpenSBI, ignored in Rust). We preserve a0, set up the stack,
// clear BSS, and call kernel_entry(handoff_ptr).
.section .text.boot
.global _start
_start:
    // Set up the stack (a0 preserved as the handoff argument).
    la   sp, __stack_top

    // Clear BSS.
    la   t0, __bss_start
    la   t1, __bss_end
1:  bgeu t0, t1, 2f
    sd   zero, 0(t0)
    addi t0, t0, 8
    j    1b
2:
    call kernel_entry
3:  wfi
    j    3b
