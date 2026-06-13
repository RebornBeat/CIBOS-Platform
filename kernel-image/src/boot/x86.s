// CIBOS kernel i686 (32-bit x86) handoff entry.
//
// CIBIOS enters protected mode (32-bit, paging per the firmware) and transfers
// control here with the handoff pointer available per the firmware contract.
// On 32-bit System V cdecl the kernel_entry(handoff_ptr: u64) argument is passed
// on the stack as two 32-bit words (low, high). The firmware places the 32-bit
// physical handoff pointer in `eax` (handoff lives in low memory, so the high
// word is zero); we push (0, eax) so the u64 argument is correct, set up the
// stack, clear BSS, and call kernel_entry.
//
// NOTE: the i686 firmware<->kernel handoff is being established; this entry
// makes the kernel image LINK for i686 (resolves _start) and follows the same
// shape as the other arches. Full bootable i686 is tracked as remaining work.

.section .text.boot
.code32
.global _start
_start:
    // Preserve the handoff pointer (eax) across the BSS clear.
    mov esi, eax

    // Set up the kernel stack.
    lea esp, [__stack_top]

    // Clear BSS: [edi, edi+ecx) = [__bss_start, __bss_end).
    lea edi, [__bss_start]
    lea ecx, [__bss_end]
    sub ecx, edi
    xor eax, eax
    rep stosb

    // Push the u64 handoff argument (high=0, low=handoff) for cdecl.
    push 0
    push esi
    call kernel_entry

.hang:
    cli
    hlt
    jmp .hang
