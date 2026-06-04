// CIBIOS AArch64 boot entry (QEMU `virt`, entered at EL1).
//
// QEMU loads the ELF and jumps to _start at the link address with the DTB
// pointer in x0. We stash x0, set up the stack, clear BSS, and call Rust.
.section .text.boot
.global _start
_start:
    // Save the DTB pointer (x0) for the Rust entry.
    adrp x1, dtb_ptr
    add  x1, x1, :lo12:dtb_ptr
    str  x0, [x1]

    // Set up the stack.
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
    bl   cibios_entry
3:  wfe
    b    3b

.section .bss
    .align 8
.global dtb_ptr
dtb_ptr: .skip 8
