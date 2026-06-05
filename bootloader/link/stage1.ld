/* bootloader/link/stage1.ld — flat MBR, exactly 512 bytes, 0xAA55 at offset 510.
 *
 * The BIOS loads the boot sector at physical 0x7C00 and jumps to its first
 * byte, so `_start` must be first and the whole image must be position-correct
 * for that origin. objcopy -O binary then yields the raw 512-byte sector. */
ENTRY(_start)
SECTIONS
{
    . = 0x7C00;
    .text : {
        *(.text._start)
        *(.text*)
        *(.rodata*)
        *(.data*)
    }
    . = 0x7C00 + 510;
    .sig : { SHORT(0xAA55); }
    /DISCARD/ : { *(.note*) *(.comment) *(.eh_frame*) *(.bss*) }
}
