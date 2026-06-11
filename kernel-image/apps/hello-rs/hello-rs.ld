/* Link the Rust hello-rs app at the CIBOS application virtual address.
 *
 * The program is small and has no mutable global state, so code and read-only
 * data are placed contiguously in a single region starting at the app base; the
 * build wraps that into one read+execute .capp segment. `_start` is the entry.
 * Any (unexpected) writable/bss output is discarded — the app uses only the
 * stack the kernel maps for it.
 */
ENTRY(_start)
SECTIONS
{
    . = 0x0000510000000000;
    .text : {
        *(.text._start)
        *(.text .text.*)
        *(.rodata .rodata.*)
    }
    /DISCARD/ : {
        *(.eh_frame*) *(.note*) *(.comment) *(.data .data.*) *(.bss .bss.*)
        *(.got*) *(.dynamic) *(.dynsym) *(.dynstr) *(.hash) *(.gnu.hash)
    }
}
