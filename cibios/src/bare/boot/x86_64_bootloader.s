// CIBIOS x86_64 boot entry — from-scratch bootloader path.
//
// Reached by the CIBOS BIOS bootloader (`bootloader/boot/stage2.s`) ALREADY in
// 64-bit long mode, under an identity-mapped page table the bootloader set up,
// with the physical address of the `BootHandoff` in RDI (System V arg 0) and a
// valid stack. Unlike the multiboot entry, there is no multiboot header and no
// 32->64 transition here — the bootloader already did that.
//
// Responsibilities: save the handoff pointer where Rust can read it (in .data,
// so the BSS clear below does not erase it), establish the firmware's own
// stack, clear BSS, and call the Rust entry `cibios_entry`.

.section .data
    .align 8
.global boot_handoff_ptr
boot_handoff_ptr: .quad 0

.section .text.boot
.code64
.global _start
_start:
    // Save the BootHandoff pointer (RDI) before anything can clobber it. It
    // lives in .data, which the BSS clear below does not touch.
    mov qword ptr [rip + boot_handoff_ptr], rdi

    // Establish the firmware boot stack (RIP-relative to avoid an absolute
    // relocation the linker rejects in this model).
    lea rsp, [rip + __stack_top]

    // Clear BSS.
    lea rdi, [rip + __bss_start]
    lea rcx, [rip + __bss_end]
    sub rcx, rdi
    xor eax, eax
    rep stosb

    // Hand off to Rust. cibios_entry is `extern "C" fn() -> !`.
    call cibios_entry

.hang:
    cli
    hlt
    jmp .hang
