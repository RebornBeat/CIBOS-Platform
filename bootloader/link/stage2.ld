/* bootloader/link/stage2.ld — flat binary loaded at 0x8000.
 *
 * Stage 1 loads Stage 2 to physical 0x8000 (segment 0, offset 0x8000) and jumps
 * to its first byte, so `stage2_start` must be first. The `.text16` section
 * (the real-mode entry) is placed ahead of everything else for that reason. */
ENTRY(stage2_start)
SECTIONS
{
    . = 0x8000;
    .text16 : { *(.text16) }
    .text   : { *(.text*) }
    .rodata : { *(.rodata*) }
    .data   : { *(.data*) }
    .bss    : { *(.bss*) *(COMMON) }
    /DISCARD/ : { *(.note*) *(.comment) *(.eh_frame*) }
}
