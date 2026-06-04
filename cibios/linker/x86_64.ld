/* CIBIOS x86_64 linker script.
 *
 * BOARD-SPECIFIC VALUES TO CONFIRM IN QEMU:
 *   - Load address `. = 1M`: standard multiboot load address. If you boot via
 *     QEMU `-kernel` with a multiboot header, 1 MiB is correct. Adjust if you
 *     chainload from another stage.
 *   - Stack size (0x10000 = 64 KiB): increase if firmware detection needs more.
 */
ENTRY(_start)

SECTIONS {
    . = 1M;                         /* FLAG: multiboot load address */

    .text : ALIGN(4K) {
        KEEP(*(.multiboot))         /* multiboot header must be early */
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
    . += 0x10000;                   /* FLAG: 64 KiB boot stack */
    __stack_top = .;

    /DISCARD/ : { *(.eh_frame) *(.note .note.*) *(.comment) }
}
