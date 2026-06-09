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

// ---- Hardware IRQ stub: timer (IRQ0 -> remapped vector 0x20) ---------------
// Same shape as the keyboard stub: save every volatile register (an IRQ can
// fire between any two instructions), call the Rust handler (which ticks the
// global counter and EOIs), restore, and iretq.
.section .text
.global timer_irq_entry
timer_irq_entry:
    push rax
    push rcx
    push rdx
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    cld
    call cibos_timer_irq
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rax
    iretq

// ---- Hardware IRQ stub: keyboard (IRQ1 -> remapped vector 0x21) ------------
// A hardware interrupt can fire between any two instructions of the interrupted
// code, so unlike the syscall path we must preserve every register the Rust
// handler might clobber (all the C-clobbered/volatile registers). We save them,
// call the handler (which reads the scancode, decodes, enqueues, and EOIs),
// restore, and iretq.
.section .text
.global keyboard_irq_entry
keyboard_irq_entry:
    push rax
    push rcx
    push rdx
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    cld
    call cibos_keyboard_irq
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rax
    iretq


// A common handler for CPU exceptions (vectors 0..31). Each per-vector entry
// pushes its vector number, then jumps here. We pass the vector and the faulting
// RIP (from the interrupt frame) to a Rust reporter that prints and halts.
// Because faults during this bring-up are fatal, this does not return.
.section .text
.global cibos_fault_common
cibos_fault_common:
    // Uniform frame after the per-vector stub: [rsp+0]=vector, [rsp+8]=errcode,
    // [rsp+16]=RIP, [rsp+24]=CS, [rsp+32]=RFLAGS, ...
    mov rdi, [rsp]          // vector
    mov rsi, [rsp + 8]      // error code
    mov rdx, [rsp + 16]     // faulting RIP
    call cibos_fault_report
1:
    cli
    hlt
    jmp 1b

// Per-vector entries. No-error-code vectors push a dummy 0 so the frame shape is
// uniform; error-code vectors leave the CPU-pushed code in place. Both then push
// the vector number on top.
.macro FAULT_NOERR vec
.global cibos_fault_\vec
cibos_fault_\vec:
    push 0              // dummy error code
    push \vec           // vector
    jmp cibos_fault_common
.endm
.macro FAULT_ERR vec
.global cibos_fault_\vec
cibos_fault_\vec:
    // CPU already pushed an error code; push the vector above it.
    push \vec
    jmp cibos_fault_common
.endm

FAULT_NOERR 0
FAULT_NOERR 1
FAULT_NOERR 2
FAULT_NOERR 3
FAULT_NOERR 4
FAULT_NOERR 5
FAULT_NOERR 6
FAULT_NOERR 7
FAULT_ERR   8
FAULT_NOERR 9
FAULT_ERR   10
FAULT_ERR   11
FAULT_ERR   12
FAULT_ERR   13
FAULT_ERR   14
FAULT_NOERR 16
FAULT_ERR   17
FAULT_NOERR 18
FAULT_NOERR 19
