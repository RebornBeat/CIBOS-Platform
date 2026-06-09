/* Link the standalone hello app at the CIBOS application virtual address.
 *
 * Everything (code + read-only data) is placed contiguously starting at the
 * app base so a single read+execute segment in the .capp covers the whole
 * program, and `_start` is first so the image entry equals the base address.
 * The program is position-independent (PC-relative data access), but linking at
 * the known base keeps the .capp entry and the loader's mapping address in
 * agreement and avoids any need for runtime relocation.
 */
ENTRY(_start)
SECTIONS
{
    . = 0x0000500000000000;
    .text : {
        *(.text._start)
        *(.text*)
        *(.rodata*)
    }
    /DISCARD/ : { *(.note*) *(.comment) *(.eh_frame*) *(.bss*) *(.data*) }
}
