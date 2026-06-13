/* CIBOS kernel i686 (32-bit x86) linker script (CIBIOS handoff path).
 *
 * Loaded by CIBIOS at 16 MiB, above CIBIOS itself (1 MiB) and within the
 * identity-mapped first 1 GiB. _start is the ELF entry; CIBIOS copies the
 * kernel component here and jumps to entry_point.
 */
ENTRY(_start)

SECTIONS {
    . = 0x1000000;                  /* FLAG: 16 MiB kernel load address */

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
