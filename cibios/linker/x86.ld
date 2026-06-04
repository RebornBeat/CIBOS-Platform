/* CIBIOS x86 (32-bit) linker script — for legacy-BIOS, older hardware.
 *
 * Multiboot1 loads a 32-bit kernel into protected mode at 1 MiB, which is
 * exactly the environment a 32-bit firmware wants — no mode switch needed.
 *
 * BOARD-SPECIFIC VALUES TO CONFIRM IN QEMU:
 *   - Load address `. = 1M`: standard multiboot load address.
 *   - Stack size (0x10000 = 64 KiB).
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
