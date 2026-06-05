# CIBOS BIOS Bootloader

A from-scratch legacy-BIOS bootloader for the x86 family. No GRUB, no multiboot.
It loads CIBIOS and the CIBOS image from the boot medium, gathers the BIOS E820
memory map, builds a `BootHandoff` (the contract in
`shared/src/protocols/boot.rs`), enters the final CPU mode, and jumps to the
CIBIOS entry point.

This is the production boot path for bare-metal and USB. ARM and RISC-V do not
use this — they boot via the platform device tree (FDT). On x86/x86_64, CIBIOS
must be built with `--features firmware-bootloader` (not the default
`firmware-multiboot`) to consume the `BootHandoff` this loader passes.

## Stages

- **Stage 1** (`boot/stage1.S`) — the 512-byte MBR. BIOS loads it at `0x7C00`.
  It checks INT 13h LBA extensions, reads the Boot Layout Descriptor from LBA 1
  to `0x0600`, validates its magic, loads Stage 2, and jumps to it.
- **Stage 2** (`boot/stage2.S`) — loaded at `0x8000`. Does all BIOS-service work
  in real mode (A20, E820, chunked disk loading via unreal-mode copies to high
  memory), builds the `BootHandoff` at `0x2000`, then:
  - **x86_64**: identity-maps 0..4 GiB with 2 MiB pages, enters long mode, jumps
    to CIBIOS with the handoff pointer in `RDI`.
  - **i686** (`-DCIBOS_BOOT32`): enters flat 32-bit protected mode (paging off),
    jumps to CIBIOS with the handoff pointer in `EAX`.

## Disk layout (produced by `tools/mkbootimage`)

```
LBA 0      Stage 1 (MBR, 512 B, ends 0xAA55)
LBA 1      Boot Layout Descriptor (one 512 B sector)
LBA 2..    Stage 2
..         CIBIOS image (flat binary)
..         CIBOS image (.cimg, opaque to the loader)
```

The descriptor (`shared::protocols::boot::BootLayoutDescriptor`) tells Stage 1
where Stage 2 is and Stage 2 where CIBIOS and CIBOS are, plus the CIBIOS entry
address to jump to. The loader treats the `.cimg` as an opaque blob: CIBIOS
parses, verifies, and places it.

## Build

```sh
./build.sh [out-dir]      # default out-dir: ./build
```

Produces `stage1.bin`, `stage2-x86_64.bin`, `stage2-i686.bin`. Uses GNU binutils
(`gcc -m32` as the assembler driver so the C preprocessor expands the `#define`
field-offset macros, plus `ld` and `objcopy`). Override with `CC=`, `LD=`,
`OBJCOPY=` (e.g. `OBJCOPY=llvm-objcopy`). The script asserts Stage 1 is exactly
512 bytes and each Stage 2 is within the 32 KiB real-mode segment window.

## ABI

The byte layout of the descriptor and handoff is pinned by the
`#[repr(C, align(8))]` types and the `const` offset assertions in
`shared/src/protocols/boot.rs`. The `BLD_*`/`HO_*` `#define`s in the assembly
mirror those offsets; if they ever drift, the Rust assertions fail the build.
`align(8)` keeps the layout identical on i686 and x86_64.

## Memory map during Stage 2

```
0x00600  Boot Layout Descriptor (from Stage 1)
0x02000  BootHandoff (88 bytes)
0x07000  real-mode stack top
0x08000  Stage 2 code/data
0x10000  E820 region array
0x20000  64 KiB disk scratch
0x30000  page tables (x86_64 only, 0x30000..0x35FFF)
0x70000  long/protected-mode stack top
```

## Status

Assembles and links clean to flat binaries of the expected size and shape; the
mode-transition encodings (the hand-encoded far jumps into long/protected mode,
the handoff-pointer register loads) are verified at the byte level. Runtime
correctness on real hardware / QEMU is part of the end-to-end hardware bring-up.
