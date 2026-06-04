// CIBIOS RISC-V 64 boot entry (QEMU `virt`, entered in S-mode by OpenSBI).
//
// OpenSBI jumps to _start with the hart id in a0 and the DTB pointer in a1.
// We stash a1, set up the stack, clear BSS, and call Rust.
.section .text.boot
.global _start
_start:
    // Save DTB pointer (a1).
    la   t0, dtb_ptr
    sd   a1, 0(t0)

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
    call cibios_entry
3:  wfi
    j    3b

.section .bss
    .align 8
.global dtb_ptr
dtb_ptr: .skip 8
