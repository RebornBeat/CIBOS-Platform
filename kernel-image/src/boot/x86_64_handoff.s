// CIBOS kernel x86_64 handoff entry.
//
// Used without the `self-boot` feature: CIBIOS has already entered 64-bit long
// mode with paging set up, and jumps here with the handoff pointer in `rdi`.
// We preserve `rdi`, set up the kernel stack, clear BSS, and call
// `kernel_entry(handoff_ptr)`.

.section .text.boot
.code64
.global _start
_start:
    // Preserve the handoff pointer across the BSS clear (which uses rdi).
    mov r15, rdi

    lea rsp, [rip + __stack_top]

    // Clear BSS.
    lea rdi, [rip + __bss_start]
    lea rcx, [rip + __bss_end]
    sub rcx, rdi
    xor eax, eax
    rep stosb

    // Restore the handoff pointer as the first argument and enter Rust.
    mov rdi, r15
    call kernel_entry

.hang:
    cli
    hlt
    jmp .hang
