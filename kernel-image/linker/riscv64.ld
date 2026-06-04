/* CIBOS kernel RISC-V 64 linker script (QEMU `virt`). */
ENTRY(_start)

SECTIONS {
    . = 0x80200000;                 /* FLAG: QEMU virt S-mode load address (after OpenSBI) */

    .text : ALIGN(4K) {
        *(.text.boot)
        *(.text .text.*)
    }
    .rodata : ALIGN(4K) { *(.rodata .rodata.*) }
    .data   : ALIGN(4K) { *(.data .data.*) }
    .bss    : ALIGN(4K) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }

    . = ALIGN(16);
    __stack_bottom = .;
    . += 0x20000;                   /* 128 KiB boot stack */
    __stack_top = .;

    /DISCARD/ : { *(.eh_frame) *(.note .note.*) *(.comment) }
}
