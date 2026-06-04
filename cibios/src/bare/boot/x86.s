// CIBIOS x86 (32-bit) boot entry — legacy BIOS / older hardware.
//
// Entered by a multiboot1 loader (e.g. QEMU `-kernel`) in 32-bit protected
// mode. Unlike the x86_64 path there is no mode switch: a 32-bit kernel runs
// directly in protected mode. Responsibilities: set the stack, save the
// multiboot info pointer, clear BSS, and call the Rust entry `cibios_entry`.

.section .multiboot, "a"
    .align 4
    .long 0x1BADB002                      // multiboot1 magic
    .long 0x00000000                      // flags
    .long -(0x1BADB002 + 0x00000000)      // checksum

.section .data
    .align 4
.global multiboot_info_ptr
multiboot_info_ptr: .long 0

.section .text.boot
.code32
.global _start
_start:
    // Boot stack.
    mov esp, offset __stack_top

    // Save the multiboot information pointer (passed in ebx) before clobbering.
    // It lives in .data so the BSS clear below does not erase it.
    mov dword ptr [multiboot_info_ptr], ebx

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
