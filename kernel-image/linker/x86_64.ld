/* CIBOS kernel x86_64 linker script.
 *
 * Load at 1 MiB (standard multiboot address for the self-boot path). For the
 * CIBIOS handoff path, CIBIOS loads the kernel image at this address.
 */
ENTRY(_start)

SECTIONS {
    . = 1M;                         /* FLAG: load address */

    .text : ALIGN(4K) {
        KEEP(*(.multiboot))         /* multiboot header (self-boot path) */
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
    . += 0x20000;                   /* FLAG: 128 KiB boot stack */
    __stack_top = .;

    /DISCARD/ : { *(.eh_frame) *(.note .note.*) *(.comment) }
}
