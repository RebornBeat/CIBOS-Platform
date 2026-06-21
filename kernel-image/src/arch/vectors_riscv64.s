// CIBOS kernel RISC-V 64 S-mode trap vector.
//
// `stvec` is set to `cibos_riscv_trap` (Direct mode) at bring-up. On any trap,
// this saves the caller-saved registers, passes scause/sepc/stval to a Rust
// reporter, and (during bring-up) halts — the equivalent of the x86 IDT fault
// reporter and the aarch64 exception vectors. Once a real runtime exists, the
// interrupt/ecall paths route to the timer/syscall handlers instead of halting.
.section .text
.balign 4
.global cibos_riscv_trap
cibos_riscv_trap:
    addi sp, sp, -(8 * 16)
    sd   ra,  (8 * 0)(sp)
    sd   t0,  (8 * 1)(sp)
    sd   t1,  (8 * 2)(sp)
    sd   t2,  (8 * 3)(sp)
    sd   a0,  (8 * 4)(sp)
    sd   a1,  (8 * 5)(sp)
    sd   a2,  (8 * 6)(sp)
    sd   a3,  (8 * 7)(sp)
    sd   a4,  (8 * 8)(sp)
    sd   a5,  (8 * 9)(sp)
    sd   a6,  (8 * 10)(sp)
    sd   a7,  (8 * 11)(sp)
    sd   t3,  (8 * 12)(sp)
    sd   t4,  (8 * 13)(sp)
    sd   t5,  (8 * 14)(sp)
    sd   t6,  (8 * 15)(sp)

    csrr a0, scause
    csrr a1, sepc
    csrr a2, stval
    call cibos_riscv_trap_report
1:  wfi
    j    1b
