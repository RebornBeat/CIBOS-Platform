// CIBOS kernel AArch64 exception vectors (EL1).
//
// The AArch64 vector table has 16 entries, each 0x80 bytes, at a 2KB-aligned
// base, covering 4 exception types (Synchronous, IRQ, FIQ, SError) x 4 source
// states (Current EL SP0, Current EL SPx, Lower EL AArch64, Lower EL AArch32).
// VBAR_EL1 is set to `cibos_vectors` at bring-up. Each entry saves the volatile
// registers, passes the exception class to a Rust reporter, and (for now) halts —
// the equivalent of the x86 IDT + fault reporter. Once a real runtime exists,
// the IRQ/sync paths route to the timer/syscall handlers instead of halting.

.section .text
.balign 2048
.global cibos_vectors
cibos_vectors:

// Macro: a vector entry that saves volatiles, calls the reporter with `kind`,
// and falls through to the common tail. Each entry must fit in 0x80 bytes.
.macro VENTRY kind
.balign 0x80
    sub  sp, sp, #(16 * 9)
    stp  x0, x1,   [sp, #(16 * 0)]
    stp  x2, x3,   [sp, #(16 * 1)]
    stp  x4, x5,   [sp, #(16 * 2)]
    stp  x6, x7,   [sp, #(16 * 3)]
    stp  x8, x9,   [sp, #(16 * 4)]
    stp  x10, x11, [sp, #(16 * 5)]
    stp  x12, x13, [sp, #(16 * 6)]
    stp  x14, x15, [sp, #(16 * 7)]
    stp  x30, xzr, [sp, #(16 * 8)]
    mov  x0, #\kind
    mrs  x1, esr_el1
    mrs  x2, elr_el1
    mrs  x3, far_el1
    bl   cibos_aarch64_exception
    b    .                       // halt: fault during bring-up is fatal
.endm

// Current EL with SP0.
VENTRY 0   // Synchronous
VENTRY 1   // IRQ
VENTRY 2   // FIQ
VENTRY 3   // SError

// Current EL with SPx.
VENTRY 4   // Synchronous
VENTRY 5   // IRQ
VENTRY 6   // FIQ
VENTRY 7   // SError

// Lower EL using AArch64.
VENTRY 8   // Synchronous
VENTRY 9   // IRQ
VENTRY 10  // FIQ
VENTRY 11  // SError

// Lower EL using AArch32.
VENTRY 12  // Synchronous
VENTRY 13  // IRQ
VENTRY 14  // FIQ
VENTRY 15  // SError
