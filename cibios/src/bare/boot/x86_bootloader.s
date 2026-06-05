// CIBIOS x86 (32-bit) boot entry — from-scratch bootloader path.
//
// Reached by the CIBOS BIOS bootloader (`bootloader/boot/stage2.s` built with
// -DCIBOS_BOOT32) ALREADY in 32-bit protected mode under a flat GDT, with the
// physical address of the `BootHandoff` in EAX and a valid stack. There is no
// multiboot header here and no mode switch — a 32-bit firmware runs directly in
// protected mode, exactly as the loader leaves it.
//
// Responsibilities: save the handoff pointer where Rust can read it (in .data,
// so the BSS clear does not erase it), establish the firmware stack, clear BSS,
// and call the Rust entry `cibios_entry`. EAX is clobbered by the BSS clear, so
// it is moved to a callee-preserved register first.

.section .data
    .align 4
.global boot_handoff_ptr
boot_handoff_ptr: .long 0

.section .text.boot
.code32
.global _start
_start:
    // Preserve the BootHandoff pointer (EAX) before the BSS clear clobbers EAX.
    mov esi, eax

    // Boot stack.
    mov esp, offset __stack_top

    // Save the handoff pointer into .data (survives the BSS clear below).
    mov dword ptr [boot_handoff_ptr], esi

    // Clear BSS.
    mov edi, offset __bss_start
    mov ecx, offset __bss_end
    sub ecx, edi
    xor eax, eax
    rep stosb

    // Hand off to Rust. cibios_entry is `extern "C" fn() -> !`.
    call cibios_entry

.hang:
    cli
    hlt
    jmp .hang
