// Enter ring 3 (user mode) via iretq.
//
// extern "C" fn enter_user_mode(entry: u64 /*rdi*/, user_stack: u64 /*rsi*/,
//                               user_code_sel: u64 /*rdx*/, user_data_sel: u64 /*rcx*/) -> !
//
// Builds the interrupt-return frame the CPU pops when transitioning to a lower
// privilege level — SS, RSP, RFLAGS, CS, RIP — and executes `iretq`, which
// switches to ring 3 and jumps to `entry` with `user_stack` as RSP. Data
// segment registers are loaded with the user data selector first. RFLAGS is set
// to a sane value with interrupts enabled (IF) and the reserved bit 1 set.

.section .text
.global enter_user_mode
enter_user_mode:
    // Load user data segment into the segment registers.
    mov ax, cx          // user_data_sel (rcx) low 16 bits
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    // Build the iretq frame (pushed in reverse: SS, RSP, RFLAGS, CS, RIP).
    push rcx            // SS  = user_data_sel
    push rsi            // RSP = user_stack
    push 0x202          // RFLAGS: IF (bit 9) | reserved bit 1
    push rdx            // CS  = user_code_sel
    push rdi            // RIP = entry

    iretq

// ---- Ring-3 entry WITH kernel-context save/restore -------------------------
// A setjmp/longjmp-style pair so a user `exit` returns control to the kernel
// instead of halting.
//
//   extern "C" fn enter_user_context(entry /*rdi*/, user_stack /*rsi*/,
//                                    user_code_sel /*rdx*/, user_data_sel /*rcx*/) -> i64
// Saves the kernel's callee-saved registers, RSP, and return address into
// KERNEL_CTX, then iretqs into ring 3. It "returns" an i64 only when
// return_to_kernel restores that context; the value is the user's exit code.
//
//   extern "C" fn return_to_kernel(code /*rdi*/) -> !
// Restores the saved kernel context so execution resumes right after the
// `call enter_user_context`, with `code` as the return value.
//
// KERNEL_CTX layout (8 quadwords):
//   [0]=rsp [1]=rbx [2]=rbp [3]=r12 [4]=r13 [5]=r14 [6]=r15 [7]=return_addr

.section .text
.global enter_user_context
enter_user_context:
    lea rax, [rip + KERNEL_CTX]
    mov [rax + 0], rsp
    mov [rax + 8], rbx
    mov [rax + 16], rbp
    mov [rax + 24], r12
    mov [rax + 32], r13
    mov [rax + 40], r14
    mov [rax + 48], r15
    mov rbx, [rsp]          // the return address pushed by `call`
    mov [rax + 56], rbx

    // Load user data segment selectors.
    mov ax, cx
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    // Build the iretq frame: SS, RSP, RFLAGS, CS, RIP.
    push rcx
    push rsi
    push 0x202
    push rdx
    push rdi
    // Hand the application its heap: the 5th/6th C args (r8 = heap base, r9 =
    // heap size) become rdi/rsi in ring 3, so _start receives (heap_base,
    // heap_size). Clear the other GPRs so no kernel state leaks to the app.
    mov rdi, r8
    mov rsi, r9
    xor rax, rax
    xor rbx, rbx
    xor rcx, rcx
    xor rdx, rdx
    xor rbp, rbp
    xor r8, r8
    xor r9, r9
    xor r10, r10
    xor r11, r11
    xor r12, r12
    xor r13, r13
    xor r14, r14
    xor r15, r15
    iretq

.global return_to_kernel
return_to_kernel:
    lea rax, [rip + KERNEL_CTX]
    mov rsp, [rax + 0]
    mov rbx, [rax + 8]
    mov rbp, [rax + 16]
    mov r12, [rax + 24]
    mov r13, [rax + 32]
    mov r14, [rax + 40]
    mov r15, [rax + 48]
    // rsp now points at the original return address (the `call` pushed it and we
    // restored rsp to that slot). Set the return value and `ret`.
    mov rax, rdi
    ret

.section .bss
.align 16
KERNEL_CTX:
    .skip 64
