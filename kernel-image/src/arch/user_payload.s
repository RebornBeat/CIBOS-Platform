// A minimal ring-3 user payload, position-independent, that issues two
// syscalls per the CIBOS ABI and then spins:
//
//   log(msg, len)    -> rax=1, rdi=msg, rsi=len, int 0x80
//   exit(0)          -> rax=2, rdi=0,   int 0x80
//
// The message bytes live immediately after the code in the same page, reached
// PC-relative so the payload works at whatever virtual address it is mapped to.
// After `exit` the kernel would normally reclaim the boundary; until that path
// exists for ring-3 tasks, the payload loops on a second log to prove it keeps
// running unprivileged (it never executes a privileged instruction).
//
// Exposed as a byte slice via the `user_payload_start`/`user_payload_end`
// symbols so the loader can copy the whole blob into a user page.

.section .rodata
.global user_payload_start
.global user_payload_end
.align 16
user_payload_start:
    // rax = Syscall::Log (1)
    mov rax, 1
    // rdi = &message (PC-relative: lea to the msg label)
    lea rdi, [rip + payload_msg]
    // rsi = message length (immediate). `offset` forces the absolute .set
    // symbol to be used as an immediate value, not a memory operand — a bare
    // symbol in GAS Intel syntax assembles as `mov rsi, [symbol]` (a load).
    mov rsi, offset payload_msg_len
    xor rdx, rdx
    int 0x80

    // exit(0): rax = Syscall::Exit (2), rdi = 0
    mov rax, 2
    xor rdi, rdi
    int 0x80

    // Should not return from exit; if it does, halt-loop without privilege.
1:
    jmp 1b

.align 8
payload_msg:
    .ascii "  [ring3] hello from an unprivileged user payload via int 0x80\n"
payload_msg_end:
user_payload_end:

// Message length as an absolute symbol (an assembly-time constant), usable as
// an immediate operand where a `symbol_end - symbol` difference is not.
.set payload_msg_len, payload_msg_end - payload_msg
