// CIBOS kernel i686 (32-bit x86) handoff entry.
//
// CIBIOS enters protected mode (32-bit, paging per the firmware) and transfers
// control here with the handoff pointer available per the firmware contract.
// kernel_entry(handoff_ptr: u64, dtb_ptr: u64) takes two u64 args; on 32-bit
// System V cdecl each u64 is two 32-bit stack words (low, high), pushed right-to-
// left. x86 has no device tree, so dtb_ptr is 0. The firmware places the 32-bit
// physical handoff pointer in `eax` (handoff lives in low memory, so its high
// word is zero); we push dtb (0,0) then handoff (0, eax), set up the stack, clear
// BSS, and call kernel_entry.
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

    // cdecl args pushed right-to-left: dtb_ptr (u64 = 0,0) first, then
    // handoff_ptr (u64 = high 0, low esi).
    push 0          // dtb_ptr high
    push 0          // dtb_ptr low
    push 0          // handoff_ptr high
    push esi        // handoff_ptr low
    call kernel_entry

.hang:
    cli
    hlt
    jmp .hang
