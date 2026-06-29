# Booting the CIBOS kernel in QEMU

The kernel image (`cibos-kernel`) is a bare-metal `no_std`/`no_main` binary. By
default it expects CIBIOS to pass a real handoff pointer in the first-argument
register (`rdi`/`x0`/`a0`) — the production, bare-metal boot pair.

For standalone QEMU testing, build with the `self-boot` feature: it synthesizes a
handoff and boots without CIBIOS, so you can run it directly under QEMU and watch
it come up over the serial console.

## Build

```sh
# handoff images (default) — booted by CIBIOS on real hardware
cargo build -p kernel-image --target x86_64-unknown-none
cargo build -p kernel-image --target aarch64-unknown-none
cargo build -p kernel-image --target riscv64gc-unknown-none-elf

# self-boot images — bootable standalone in QEMU (testing only)
cargo build -p kernel-image --features self-boot --target x86_64-unknown-none
```

Binaries land in `target/<triple>/debug/cibos-kernel`.

## Run (self-boot)

x86_64 (multiboot1 via `-kernel`):

```sh
qemu-system-x86_64 \
  -kernel target/x86_64-unknown-none/debug/cibos-kernel \
  -serial stdio -display none
```

AArch64 (`virt`, entered at EL1; PL011 serial):

```sh
# aarch64 boots as a raw ARM64 Image (the kernel carries the standard ARM64 image
# header), so QEMU passes the DTB pointer in x0 exactly as real firmware/U-Boot do.
# Build the Image first: ./build-arm64-image.sh
qemu-system-aarch64 -machine virt -cpu cortex-a72 -m 256M \
  -kernel target/aarch64-unknown-none/debug/Image \
  -serial stdio -display none
```

RISC-V 64 (`virt`, OpenSBI loads the kernel at 0x80200000; SBI console):

```sh
qemu-system-riscv64 -machine virt -m 256M \
  -kernel target/riscv64gc-unknown-none-elf/debug/cibos-kernel \
  -serial stdio -display none
```

## Expected serial output

```
CIBOS kernel: entry
CIBOS kernel: heap online (8388608 bytes)
CIBOS kernel: handoff accepted, 134217728 bytes usable across 1 region(s)
CIBOS kernel: init lane running
CIBOS kernel: scheduler idle after 1 poll(s)
CIBOS kernel: boot complete
CIBOS kernel: halt
```

The `init lane running` line is the proof that the real scheduler ran a real
lane on the booted kernel. After `halt` the CPU idles; stop QEMU with
`Ctrl-A x` (x86/arm) or `Ctrl-C`.

## Boot-address flags to confirm per board

* **x86_64** — load at 1 MiB (`linker/x86_64.ld`), standard for multiboot.
* **aarch64** — load at `0x40080000` for QEMU `virt`; confirm against the DTB on
  other boards.
* **riscv64** — load at `0x80200000`, the S-mode entry after OpenSBI on QEMU
  `virt`.

## Notes

* The 8 MiB kernel heap is a static region inside the image; the handoff memory
  map drives the kernel's *accounting*, not this heap.
* The self-boot handoff uses nominal values (1 core, 128 MiB usable). Under
  CIBIOS, the real topology and memory map flow through instead.
