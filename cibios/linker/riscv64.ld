/* CIBIOS RISC-V 64 linker script.
 *
 * BOARD-SPECIFIC VALUES TO CONFIRM IN QEMU:
 *   - Load address 0x80200000: on the QEMU `virt` machine, OpenSBI occupies
 *     0x80000000 and hands control to the S-mode payload at 0x80200000.
 *     Confirm with `qemu-system-riscv64 -machine virt -bios default`.
 *   - Stack size (0x10000 = 64 KiB).
 */
ENTRY(_start)

SECTIONS {
    . = 0x80200000;                 /* FLAG: QEMU virt S-mode payload address */

    .text : ALIGN(4K) {
        KEEP(*(.text.boot))
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
