/* CIBOS kernel AArch64 linker script (QEMU `virt`). */
ENTRY(_start)

SECTIONS {
    . = 0x40080000;                 /* FLAG: QEMU virt kernel load address */

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
