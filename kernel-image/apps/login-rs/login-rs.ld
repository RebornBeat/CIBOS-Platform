/* Link the Rust hello-rs app at the CIBOS application virtual address.
 *
 * The large code model emits `l`-prefixed sections (.ltext/.lrodata/.ldata/
 * .lbss). Code + read-only data go in one read+execute region at the app base;
 * writable data + bss (the cibos-app allocator's state) go in a separate region
 * the build wraps as a read+write .capp segment, page-aligned so the two
 * segments map cleanly. `_start` is the entry.
 */
ENTRY(_start)
SECTIONS
{
    . = 0x0000520000000000;
    .text : {
        *(.text._start)
        *(.ltext._start)
        *(.text .text.*)
        *(.ltext .ltext.*)
        *(.rodata .rodata.*)
        *(.lrodata .lrodata.*)
    }

    /* Page-align the writable region so it becomes its own .capp segment. */
    . = ALIGN(0x1000);
    __data_start = .;
    .data : {
        *(.data .data.*)
        *(.ldata .ldata.*)
        *(.got .got.*)
        *(.data.rel.ro .data.rel.ro.*)
    }
    .bss : {
        *(.bss .bss.*)
        *(.lbss .lbss.*)
        *(COMMON)
    }
    __data_end = .;

    /DISCARD/ : {
        *(.eh_frame*) *(.note*) *(.comment)
        *(.dynamic) *(.dynsym) *(.dynstr) *(.hash) *(.gnu.hash)
    }
}
