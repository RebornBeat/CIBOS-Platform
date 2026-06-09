// x86_64 syscall trap stub (vector 0x80).
//
// On entry the CPU has pushed the interrupt frame (SS, RSP, RFLAGS, CS, RIP).
// The syscall ABI passes: number=rax, arg0=rdi, arg1=rsi, arg2=rdx. We marshal
// those into the System V C call convention for `cibos_syscall_handler`:
//   rdi=number, rsi=arg0, rdx=arg1, rcx=arg2
// call it, place the returned i64 (in rax) into the saved frame so it reaches
// the caller, restore registers, and iretq.
//
// We save/restore the caller-clobbered registers around the call so the
// interrupted code is undisturbed except for rax (the documented return reg).

.section .text
.global syscall_trap_entry
syscall_trap_entry:
    // Save registers we will clobber (and that the callee may clobber).
    push rdi
    push rsi
    push rdx
    push rcx
    push r8
    push r9
    push r10
    push r11

    // Marshal ABI regs -> SysV arg regs. rax/rdi/rsi/rdx still hold the ABI
    // values (rdi/rsi/rdx were just saved, not modified).
    mov rcx, rdx          // arg2 -> 4th C arg (rcx)
    mov rdx, rsi          // arg1 -> 3rd C arg (rdx)
    mov rsi, rdi          // arg0 -> 2nd C arg (rsi)
    mov rdi, rax          // number -> 1st C arg (rdi)

    call cibos_syscall_handler
    // Return value now in rax; stash it so the pop sequence does not lose it,
    // by leaving it in rax (we restore everything *except* rax).

    pop r11
    pop r10
    pop r9
    pop r8
    pop rcx
    pop rdx
    pop rsi
    pop rdi

    iretq
