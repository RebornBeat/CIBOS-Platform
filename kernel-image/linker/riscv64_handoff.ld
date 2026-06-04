/* CIBOS kernel RISC-V 64 linker script (CIBIOS handoff path). */
ENTRY(_start)

SECTIONS {
    . = 0x81000000;                 /* above CIBIOS (0x80200000) on QEMU virt */

    .text : ALIGN(4K) { *(.text.boot) *(.text .text.*) }
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
    . += 0x20000;
    __stack_top = .;

    /DISCARD/ : { *(.eh_frame) *(.note .note.*) *(.comment) }
}
