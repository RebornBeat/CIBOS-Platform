/* CIBIOS AArch64 linker script.
 *
 * BOARD-SPECIFIC VALUES TO CONFIRM IN QEMU:
 *   - Load address 0x40080000: the QEMU `virt` machine's default kernel load
 *     address (RAM starts at 0x40000000; QEMU places a `-kernel` ELF at
 *     0x40080000). Confirm with `qemu-system-aarch64 -machine virt`.
 *   - Stack size (0x10000 = 64 KiB).
 */
ENTRY(_start)

SECTIONS {
    . = 0x40080000;                 /* FLAG: QEMU virt kernel load address */

    .text : ALIGN(4K) {
        KEEP(*(.text.boot))         /* _start entry first */
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
