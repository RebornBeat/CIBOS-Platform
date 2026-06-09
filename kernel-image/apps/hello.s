/* CIBOS sample application: "hello" (bare ring-3, position-independent).
 *
 * This is a *standalone* user program — not linked into the kernel. The build
 * script assembles it, links it at the application virtual address, objcopies
 * it to a flat binary, and wraps it in a `.capp` image (one read+execute
 * segment, entry = `_start`). The kernel loads that `.capp` through
 * `loader::run_app_image`, mapping the segment into a user address space and
 * entering ring 3 at `_start`.
 *
 * It issues two syscalls per the CIBOS ABI and exits:
 *   log(msg, len)  -> rax=1, rdi=msg, rsi=len, int 0x80
 *   exit(7)        -> rax=2, rdi=7,   int 0x80
 *
 * The message is reached PC-relative so the program is position-independent;
 * the exit code (7) is distinctive so the kernel can confirm it received this
 * app's exit value (not a payload's) across the ring boundary.
 */

.intel_syntax noprefix
.section .text
.global _start
_start:
    mov rax, 1                      /* Syscall::Log */
    lea rdi, [rip + hello_msg]      /* message pointer (PC-relative) */
    mov rsi, offset hello_msg_len   /* message length (immediate) */
    xor rdx, rdx
    int 0x80

    mov rax, 2                      /* Syscall::Exit */
    mov rdi, 7                      /* exit code 7 (distinctive) */
    int 0x80

    /* Should not return from exit; spin without privilege if it does. */
1:
    jmp 1b

.section .rodata
.align 8
hello_msg:
    .ascii "  [app:hello] external .capp running in ring 3, says hi\n"
hello_msg_end:
.set hello_msg_len, hello_msg_end - hello_msg
