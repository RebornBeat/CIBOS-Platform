// CIBOS kernel AArch64 boot entry (QEMU `virt`, EL1).
//
// Entered with the handoff pointer in x0 (CIBIOS path) or the DTB pointer (QEMU
// self-boot, ignored in Rust). We preserve x0, set up the stack, clear BSS, and
// call kernel_entry(handoff_ptr).
.section .text.boot
.global _start
_start:
    // Set up the stack (x0 preserved as the handoff argument).
    adrp x1, __stack_top
    add  x1, x1, :lo12:__stack_top
    mov  sp, x1

    // Clear BSS.
    adrp x1, __bss_start
    add  x1, x1, :lo12:__bss_start
    adrp x2, __bss_end
    add  x2, x2, :lo12:__bss_end
1:  cmp  x1, x2
    b.ge 2f
    str  xzr, [x1], #8
    b    1b
2:
    bl   kernel_entry
3:  wfe
    b    3b
