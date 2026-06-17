// Per-lane ring-3 context save + resume (x86_64).
//
// This is the symmetric partner of enter_user.s's kernel-context longjmp.
// enter_user.s lets a user `exit` return to the kernel. Here we capture a
// user lane's FULL architectural state at a trap so the kernel can PARK it and
// later resume ANY parked lane exactly where it trapped — the load-bearing
// mechanism for live ring-3 multi-context (spawn) and cross-boundary channel
// enforcement.
//
// IMPORTANT — arbitrary-lane by construction (no single-slot shortcut):
// The save path does NOT write into a fixed buffer. It writes into the context
// of whichever lane is *currently running*, found through the kernel-set
// pointer `CURRENT_USER_CTX`. The selector sets this pointer to the running
// lane's `SavedUserContext` (which lives in the selector-owned Ring3Lane table)
// each time it dispatches/resumes a lane — exactly the "current task" pointer
// real kernels use. With N lanes, only the pointer changes per dispatch; this
// assembly is unchanged. This keeps step 2 of the live-context design ("resume
// an ARBITRARY parked lane") faithful from the first increment.
//
// Routines:
//
//   user_ctx_trap_entry  — the context-saving int-0x80 stub. On entry the CPU
//     has pushed the iretq frame (SS,RSP,RFLAGS,CS,RIP). We save the full user
//     GP register file PLUS those frame fields into [*CURRENT_USER_CTX], then
//     marshal the ABI regs and call `cibos_user_trap_handler`. If the handler
//     returns (ordinary syscall), we write its i64 result into the saved RAX and
//     resume the SAME lane inline. If it PARKS the lane, it longjmps to the
//     kernel via return_to_kernel, leaving [*CURRENT_USER_CTX] resumable.
//
//   resume_ring3(ctx /*rdi*/) -> !   — load *ctx and iretq back to ring 3.
//   resume_user_context(ctx /*rdi*/, kctx /*rsi*/) -> i64 — save the kernel
//     return context into the caller-supplied `kctx` (a KernelReturnContext, NOT
//     a global), then resume *ctx. The resumed lane's `exit` returns here via
//     `return_to_saved_kernel`. Per-resume kctx keeps nested resumes correct.

.set OFF_R15,    0
.set OFF_R14,    8
.set OFF_R13,    16
.set OFF_R12,    24
.set OFF_R11,    32
.set OFF_R10,    40
.set OFF_R9,     48
.set OFF_R8,     56
.set OFF_RBP,    64
.set OFF_RDI,    72
.set OFF_RSI,    80
.set OFF_RDX,    88
.set OFF_RCX,    96
.set OFF_RBX,    104
.set OFF_RAX,    112
.set OFF_RIP,    120
.set OFF_CS,     128
.set OFF_RFLAGS, 136
.set OFF_RSP,    144
.set OFF_SS,     152

.section .text
.global user_ctx_trap_entry
user_ctx_trap_entry:
    // Save into the CURRENT lane's context (kernel-set pointer), not a fixed
    // buffer. Preserve the live r11 (used as scratch base) on the stack first.
    push r11
    // r11 = *CURRENT_USER_CTX  (the running lane's SavedUserContext pointer)
    mov r11, [rip + CURRENT_USER_CTX]

    mov [r11 + OFF_R15], r15
    mov [r11 + OFF_R14], r14
    mov [r11 + OFF_R13], r13
    mov [r11 + OFF_R12], r12
    mov [r11 + OFF_R10], r10
    mov [r11 + OFF_R9],  r9
    mov [r11 + OFF_R8],  r8
    mov [r11 + OFF_RBP], rbp
    mov [r11 + OFF_RDI], rdi
    mov [r11 + OFF_RSI], rsi
    mov [r11 + OFF_RDX], rdx
    mov [r11 + OFF_RCX], rcx
    mov [r11 + OFF_RBX], rbx
    mov [r11 + OFF_RAX], rax
    // Recover the original r11 we pushed, store it, drop the slot.
    mov rax, [rsp]
    mov [r11 + OFF_R11], rax
    add rsp, 8

    // rsp now points at the CPU-pushed iretq frame. Copy frame fields.
    mov rax, [rsp + 0]              // RIP
    mov [r11 + OFF_RIP], rax
    mov rax, [rsp + 8]              // CS
    mov [r11 + OFF_CS], rax
    mov rax, [rsp + 16]            // RFLAGS
    mov [r11 + OFF_RFLAGS], rax
    mov rax, [rsp + 24]            // RSP (user)
    mov [r11 + OFF_RSP], rax
    mov rax, [rsp + 32]            // SS
    mov [r11 + OFF_SS], rax

    // Marshal ABI regs -> SysV args for the Rust handler (read from the saved
    // context so earlier clobbers don't matter):
    //   number=rax->rdi, arg0=rdi->rsi, arg1=rsi->rdx, arg2=rdx->rcx
    mov rdi, [r11 + OFF_RAX]
    mov rsi, [r11 + OFF_RDI]
    mov rdx, [r11 + OFF_RSI]
    mov rcx, [r11 + OFF_RDX]

    call cibos_user_trap_handler
    // Ordinary syscall: write result into saved RAX and resume THIS lane inline.
    // (If the handler parked the lane, control never reaches here.)
    mov r11, [rip + CURRENT_USER_CTX]
    mov [r11 + OFF_RAX], rax
    mov rdi, r11
    jmp resume_ring3_from

// resume_user_context(ctx /*rdi*/, kctx /*rsi*/) -> i64
// Save the kernel return context into the caller-supplied KernelReturnContext
// (*kctx, 8 quadwords: rsp,rbx,rbp,r12,r13,r14,r15,retaddr) so the resumed
// lane's `exit` returns to right after THIS call, then resume *ctx. Using a
// caller-supplied kctx (not a global) means multiple resumes nest correctly.
.global resume_user_context
resume_user_context:
    mov [rsi + 0], rsp
    mov [rsi + 8], rbx
    mov [rsi + 16], rbp
    mov [rsi + 24], r12
    mov [rsi + 32], r13
    mov [rsi + 40], r14
    mov [rsi + 48], r15
    mov rax, [rsp]                 // return address pushed by `call`
    mov [rsi + 56], rax
    // Record which kctx the next return_to_saved_kernel should use.
    mov [rip + ACTIVE_KERNEL_CTX], rsi
    // rdi already = ctx; fall through to the shared restore path.
    jmp resume_ring3_from

// return_to_saved_kernel(code /*rdi*/) -> !
// Restore the kernel context recorded by the most recent resume_user_context
// (via ACTIVE_KERNEL_CTX) and return `code` as that call's result. Invoked by
// the trap handler when a RESUMED lane exits.
.global return_to_saved_kernel
return_to_saved_kernel:
    mov rsi, [rip + ACTIVE_KERNEL_CTX]
    mov rsp, [rsi + 0]
    mov rbx, [rsi + 8]
    mov rbp, [rsi + 16]
    mov r12, [rsi + 24]
    mov r13, [rsi + 32]
    mov r14, [rsi + 40]
    mov r15, [rsi + 48]
    mov rax, rdi                   // return value (exit code)
    ret

// resume_ring3(ctx /*rdi*/) -> !  : resume an arbitrary parked context.
.global resume_ring3
resume_ring3:
resume_ring3_from:
    // rdi = pointer to SavedUserContext. Make it CURRENT (so a later trap saves
    // back into the same lane), rebuild the iretq frame, load the GP file, iretq.
    mov [rip + CURRENT_USER_CTX], rdi
    mov rax, [rdi + OFF_SS]
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    // Push iretq frame: SS, RSP, RFLAGS, CS, RIP (reverse order).
    push qword ptr [rdi + OFF_SS]
    push qword ptr [rdi + OFF_RSP]
    push qword ptr [rdi + OFF_RFLAGS]
    push qword ptr [rdi + OFF_CS]
    push qword ptr [rdi + OFF_RIP]

    // Load the GP file (rdi last, since we index through it).
    mov r15, [rdi + OFF_R15]
    mov r14, [rdi + OFF_R14]
    mov r13, [rdi + OFF_R13]
    mov r12, [rdi + OFF_R12]
    mov r11, [rdi + OFF_R11]
    mov r10, [rdi + OFF_R10]
    mov r9,  [rdi + OFF_R9]
    mov r8,  [rdi + OFF_R8]
    mov rbp, [rdi + OFF_RBP]
    mov rsi, [rdi + OFF_RSI]
    mov rdx, [rdi + OFF_RDX]
    mov rcx, [rdi + OFF_RCX]
    mov rbx, [rdi + OFF_RBX]
    mov rax, [rdi + OFF_RAX]
    mov rdi, [rdi + OFF_RDI]        // rdi last

    iretq

// Kernel-set pointers (NOT context storage — the contexts live in the
// selector-owned Ring3Lane table). CURRENT_USER_CTX -> the running lane's
// SavedUserContext; ACTIVE_KERNEL_CTX -> the KernelReturnContext the next
// resumed-lane exit should unwind to. Both are single pointers because exactly
// one ring-3 lane runs at a time (cooperative, single selector) — switching
// lanes just repoints them; it does not serialize the lane storage.
.section .bss
.align 8
.global CURRENT_USER_CTX
CURRENT_USER_CTX:
    .skip 8
.global ACTIVE_KERNEL_CTX
ACTIVE_KERNEL_CTX:
    .skip 8
