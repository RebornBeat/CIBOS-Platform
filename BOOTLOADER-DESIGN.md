# From-Scratch BIOS Bootloader — Integration Design

**Status:** design locked from a full read of the existing boot path. No GRUB, no
multiboot dependency on the production path (decision 6, option a). Mirrors the
proven `self-boot` vs handoff split that `kernel-image` already uses.

## Verified baseline (this session, on disk in `/home/claude/cibos-workspace`)

- Host test suite: **218 passed / 0 failed** (`cargo test --workspace`).
- Bare-metal builds OK: `kernel-image` and `cibios` on `x86_64-unknown-none`.
- Toolchain: stable 1.96 at `/root/.cargo`; targets `x86_64-unknown-none`,
  `aarch64-unknown-none`, `riscv64gc-unknown-none-elf` installed.
- The bootloader work from the prior chat turn was **never written to disk** —
  no `bootloader/`, no `shared/src/protocols/boot.rs`, no `tools/mkbootimage/`.
  This session starts it for real.

## The existing boot flow (what the bootloader must bind to)

CIBIOS x86_64 today is entered **in 32-bit protected mode by a multiboot1
loader** (QEMU `-kernel`). Chain of facts, all read from source:

1. `cibios/src/bare/boot/x86_64.s` — `.multiboot` header; `_start` (`.code32`)
   saves `ebx` → `multiboot_info_ptr`, clears BSS, builds a 1 GiB identity map,
   switches to long mode, far-jumps to `_start64`, calls `cibios_entry`.
2. `cibios/src/bare/arch/x86_64.rs`:
   - `detect()` → `read_multiboot_memory()` reads the mmap from the multiboot
     info structure pointed to by `multiboot_info_ptr`.
   - `locate_image()` reads the **first multiboot module** as the CIBOS `.cimg`.
   - `jump_to_kernel(entry, handoff_ptr)` puts handoff in `rdi`, jumps to kernel.
3. `cibios/src/bare/mod.rs::run()` → `boot_image()`:
   - verifies the `.cimg` (`verify_image`), then **`ImageView::parse` +
     `for_each_component` copies each component to its own `load_addr`** — so the
     bootloader does NOT need the CIBOS image's internal layout or entry point.
     The `.cimg` is an **opaque blob** to the bootloader.
   - builds the CIBIOS→CIBOS `HandoffData` and `jump_to_kernel`s.

**Consequence:** the bootloader's only responsibilities for CIBOS are to (a) load
the raw `.cimg` blob into RAM and (b) tell CIBIOS where it is. CIBIOS already owns
parse/verify/place/jump. This is much smaller than the over-specified
`BootHandoff` sketched in the prior chat turn (no `cibos_load_addr`, no
`cibos_entry`, no duplicated page-table story in the descriptor).

## The integration: a `firmware-bootloader` entry path in CIBIOS

Mirror `kernel-image`'s `self-boot` split exactly, one level down:

- **`firmware-multiboot`** (opt-in, QEMU `-kernel`): the current 32-bit
  `_start`, `multiboot_info_ptr`, `read_multiboot_memory`, multiboot
  `locate_image`. Unchanged behavior, now behind a feature.
- **default = bootloader path**: a new 64-bit `_start` that our Stage 2 jumps to
  with `rdi = &BootHandoff`. `detect()` and `locate_image()` read the memory map
  and CIBOS blob from the `BootHandoff` instead of multiboot structures.
  `cibios_entry`, `run()`, `boot_image()`, `jump_to_kernel()` are **unchanged** —
  only the *source* of (memory map, image bytes) swaps.

The bootloader does the 32→64 transition + identity map and jumps to CIBIOS's
64-bit entry, so CIBIOS's bootloader-path `_start` is a thin 64-bit stub (stack +
BSS clear + save `rdi` + call `cibios_entry`), NOT the 32-bit multiboot dance.

## Disk image layout (mkbootimage output, 512-byte sectors)

```
LBA 0      Stage 1 (MBR, 512 B, ends 0xAA55)
LBA 1      BootLayoutDescriptor (one sector)
LBA 2..    Stage 2 (x86_64 long-mode or i686 pm)
..         CIBIOS image (flat binary from objcopy of the cibios ELF)
..         CIBOS image (.cimg, opaque blob)
```

Stage 1 reads the descriptor (LBA 1) to find Stage 2; Stage 2 reads it to find
CIBIOS + CIBOS, loads both to their physical addresses, gathers E820, builds the
`BootHandoff`, enters long mode, jumps to the CIBIOS entry with `rdi=&handoff`.

## The contract (`shared/src/protocols/boot.rs`, new)

`#[repr(C)]` with compile-time offset asserts (the asm and the host image tool
bind to these exact offsets):

- `BootMemoryRegion` — 24 B, byte-identical to a BIOS E820 record.
- `BootHandoff` — magic/version/flags/boot_drive, memory map ptr+count, the
  CIBOS blob `(addr,size)`, the CIBIOS blob `(addr,size)`, page-table root,
  Stage 2 `(addr,size)`. (No CIBOS load addr/entry — CIBIOS owns that.)
- `BootLayoutDescriptor` — on-disk: stage2/cibios/cibos `(lba, sectors,
  load_addr)` + cibios_entry (the CIBIOS entry the loader jumps to) + cibos exact
  byte size.

## Why the CIBIOS entry is in the descriptor but the CIBOS entry is not

The loader must jump to **CIBIOS**, so it needs CIBIOS's physical entry address
(`cibios_entry`, filled by mkbootimage from the CIBIOS ELF entry). It must NOT
jump to CIBOS — CIBIOS does — so no CIBOS entry is needed anywhere in the loader.

## Build/verify plan for this layer

1. `shared/src/protocols/boot.rs` + register in `protocols/mod.rs` + re-export
   in `lib.rs`. Host-testable (offset asserts + unit tests). **DONE, verified.**
   - The structs are `#[repr(C, align(8))]`, not bare `#[repr(C)]`: on i686
     `u64` aligns to 4, which would change padding/size and break the wire
     contract between a 32-bit bootloader and CIBIOS. `align(8)` pins identical
     layout on i686 and x86_64 (verified by building the const asserts on both).
2. CIBIOS `firmware-multiboot` (default) vs `firmware-bootloader` features +
   build.rs mutual-exclusion and "x86 needs a boot entry" checks; new 64-bit
   `boot/x86_64_bootloader.s` and 32-bit `boot/x86_bootloader.s` entries that
   save the `BootHandoff` pointer (rdi / eax); `bare/mod.rs` selects boot asm by
   feature; `arch/x86_64.rs` and `arch/x86.rs` gained cfg-split `locate_image`,
   `read_memory_map`, and a `boot_handoff()` reader (multiboot vs handoff source)
   while `detect`/`putc`/`halt`/`jump_to_kernel`/`gather_entropy` stayed shared.
   `bare/mod.rs::run()`/`boot_image()` UNCHANGED. **DONE, verified** on x86_64 +
   i686 (both paths), ARM/RISC-V unaffected, host suite 223/0, clippy clean.
3. `bootloader/` — stage1.S (512-byte MBR), stage2.S (real-mode A20/E820/chunked
   disk load → long mode for x86_64 / protected mode for i686, building the
   BootHandoff at 0x2000 and jumping to the CIBIOS entry), link/stage1.ld,
   link/stage2.ld, build.sh, README.md. **DONE, verified.** Toolchain is GNU
   binutils (`gcc -m32` driver for cpp + `as`, then `ld` + `objcopy`); `clang`/
   `ld.lld` are NOT installed, so the prior plan's clang dependency was dropped.
   Built clean: stage1.bin = 512 B (0xAA55 at 510), stage2-x86_64.bin = 1133 B,
   stage2-i686.bin = 925 B (both well under the 32 KiB real-mode ceiling). The
   two hand-encoded far jumps were disassembled and verified: x86_64 `66 ea ..
   .. 08` → `jmp 0x8:long_entry` then 64-bit `mov rdi,0x2000` / `mov rax,[0x640]`
   / `jmp rax`; i686 → `jmp 0x8:pm_entry` then `mov eax,0x2000` / `jmp ebx`.
   The handoff pointer lands in RDI (x86_64) / EAX (i686), matching the CIBIOS
   bootloader entries from Layer 2.
4. `tools/mkbootimage/` — host crate (depends on `shared` with `std`) that reads
   stage1/stage2/cibios/cibos, computes sector-aligned LBAs and the descriptor,
   and writes the bootable `.img`. The descriptor is serialized via a new
   `BootLayoutDescriptor::to_bytes`/`from_bytes` added to `shared` (fields in
   declared = repr(C) order through the existing `ByteWriter`/`ByteReader`), so
   the on-disk bytes cannot drift from the type. Added to workspace members.
   **DONE, verified.** Validates stage1 is exactly 512 B ending in 0xAA55 and
   stage2 <= 32 KiB. End-to-end smoke test passed: bootloader build → CIBIOS
   bare (bootloader path) → objcopy flat → mkimage `.cimg` → mkbootimage `.img`;
   the produced image round-trips through `shared::from_bytes`, the magic and
   signature are correct, and the CIBIOS bytes land at the LBA the descriptor
   claims. Host suite 225/0, clippy clean.

   **Entry-point finding (important for Layer 5):** the CIBIOS flat binary loads
   at 1 MiB but its `_start` is NOT at the base — for the current build the ELF
   entry is `0x1004e5`. So the Layer 5 wrapper MUST read the real entry from the
   CIBIOS ELF (`readelf -h ... | awk '/Entry point/{print $NF}'`) and pass it as
   `--cibios-entry`; the mkbootimage default (`entry = load`) is only correct
   when `_start` happens to sit at the load base.
5. Wrapper script to produce a bootable `.img` for a given profile/arch.
   **DONE, verified.** `build-bootimage.sh <profile> [arch...]` drives the whole
   chain end-to-end: build the bootloader, build the CIBOS kernel image stamped
   with the operational profile → flat binary → `.cimg` (via mkimage), build
   CIBIOS with `firmware-bootloader` + the matching firmware features → flat
   binary, read the CIBIOS ELF entry, and assemble the `.img` with mkbootimage.
   Verified: `compute` and `performance` each produce a complete flashable
   `images/cibos-<profile>-x86_64.img` (validated: 0xAA55 sig, descriptor
   round-trips via `shared::from_bytes`, CIBIOS bytes match at their LBA, CIBOS
   `.cimg` magic present at its LBA). The wrapper reads the real ELF entry per
   build (saw `0x100fac`, `0x101bf6`) rather than assuming load==entry.

   Honest pairing (mirrors `build-profile.sh`): only Lightweight-compatible
   profiles (`compute`, `performance`) yield a complete bare image today;
   `maximum-isolation`/`balanced` require Standard (signed) firmware, which does
   not link bare until the no_std SPHINCS+ verifier — the wrapper refuses with a
   clear message rather than emit a broken image.

   **i686 `.img` is NOT yet buildable** — recorded so it isn't lost. Two
   prerequisites, both OUTSIDE the bootloader layers (which all build for i686):
   (1) `mkimage` accepts only x86_64/aarch64/riscv64 arch tags and cannot stamp
   the `x86` (32-bit) tag (`ProcessorArchitecture::X86 = 3`), so CIBIOS would
   reject an i686 `.cimg` as wrong-arch at boot; (2) `kernel-image`'s 32-bit x86
   arch backend is incomplete (missing `arch::putc`/`init_serial`/`halt`, two
   type mismatches), so the kernel image does not compile for i686-cibos-none.
   CIBIOS, the boot contract, the bootloader, and mkbootimage all handle i686;
   the gate is the kernel image and the mkimage arch tag. The wrapper refuses
   i686 with this explanation.

## Console driver (post-bootloader critical-path item) — DONE for x86_64

A COM1 **serial** console already existed in both CIBIOS and the kernel; what was
missing was **on-screen** output. Added a VGA text-mode console for the kernel:
- `kernel-image/src/arch/vga.rs` — no_std VGA text driver (0xB8000, 80×25),
  cursor, newline/tab handling, hardware scroll, CRTC cursor updates. x86_64-only
  (gated in `arch/mod.rs`); aarch64/riscv64 keep serial (framebuffer console for
  those is a later step).
- `arch/x86_64.rs`: `init_serial` now also clears/init's VGA; `putc` writes to
  BOTH COM1 and VGA. The public arch interface (`init_serial`/`putc`/`halt`) is
  unchanged, so `boot.rs` is untouched. Builds + clippy clean on all three arches
  and self-boot; host suite 225/0.

## FIRST RUNTIME VALIDATION (QEMU) — boot chain works; one bug found

Installed QEMU and booted the real `mkbootimage` disk image
(`-drive format=raw`). The from-scratch boot chain WORKS end to end on emulated
hardware — serial proves it:
```
CIBIOS v0.1.0 starting
detected: 1 core(s), 127 MiB RAM at 0x100000      (our Stage 2 E820 -> BootHandoff)
firmware profile: Lightweight
CIBOS image found (414128 bytes); booting          (our blob, exact size)
image verified (signature skipped), entry 0x1000000
components placed
handoff built; transferring control to CIBOS
```
BIOS -> our MBR -> Stage 2 (A20/E820/load/long-mode) -> CIBIOS (reads
BootHandoff, finds+verifies+places CIBOS) -> CIBIOS->CIBOS handoff. Layers 1-5
are runtime-confirmed working together.

**Open bug (next session): the CIBIOS->CIBOS jump does not land in the kernel.**
**[FIXED — runtime-verified.]** Root cause: a register-allocation collision in
`jump_to_kernel`'s inline asm. The old form `mov rdi, {handoff}; jmp {entry}`
let the allocator place `{entry}` in RDI too, so the `mov` clobbered the entry
address and the `jmp` went to the handoff pointer (a stack address ~0x12ed40) —
triple-fault. Fix: pin the handoff pointer to the arg register with an explicit
`in("rdi")` (x86_64) / `in("eax")` (x86) / `in("x0")` (aarch64) / `in("a0")`
(riscv64) constraint and let `{entry}` use any other register; the `mov` is gone.
Verified codegen now does `mov handoff->rdi; jmp *entry`. The SAME bug existed in
all four arches' `jump_to_kernel`; all four fixed. After the fix, the compute
image boots fully in QEMU — serial AND VGA both show:
```
CIBOS kernel: entry
CIBOS kernel: heap online (8388608 bytes)
CIBOS kernel: handoff accepted, 133692416 bytes usable across 1 region(s)
CIBOS kernel: init lane running          (async scheduler ran a task)
CIBOS kernel: scheduler idle after 1 poll(s)
CIBOS kernel: boot complete
```
So the full from-scratch path BIOS -> MBR -> Stage 2 -> CIBIOS -> CIBOS kernel ->
weighted-entropy scheduler is runtime-confirmed, with on-screen (VGA) output.
Repeatable via `qemu-boot.sh compute`.

## Layer status summary

All five bootloader-path layers are complete and verified on x86_64:
contract (shared/boot.rs) → CIBIOS entry wiring → bootloader (stage1/stage2) →
mkbootimage → build-bootimage.sh wrapper. Host suite 225/0 throughout. The full
chain has been exercised end-to-end as a single command producing a validated
flashable image. Runtime correctness on real hardware / QEMU is the deferred
end-to-end bring-up step (accepted). The critical path now continues with the
console driver, the no_std SPHINCS+ verifier (which unblocks Standard firmware
and the maximum-isolation/balanced bare images), MMU/hardware isolation, the app
loader, syscall transport, behavioral-flag implementations, and on-kernel
examples.

Each step keeps the host suite green and the bare targets building before moving
on. Bare-metal runtime correctness is validated on hardware at the end (accepted).

## Note on disk space

The four-target cross-compiles + build-std + host suite fill `target/` (~8 GiB).
If a build fails with `os error 28` / `ld signal 7`, run `cargo clean` — the
`target/` dir is fully regenerable — then rebuild.
