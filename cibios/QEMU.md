# CIBIOS — Build & QEMU Bring-Up Guide

This guide covers building the CIBIOS firmware for each supported architecture
and booting it under QEMU. The firmware logic is verified on the host
(`cargo test`); this document is the bare-metal half — booting a real image.

## Prerequisites

```sh
# Bare-metal targets (no nightly needed; these are tier-2 with prebuilt core/alloc)
rustup target add x86_64-unknown-none aarch64-unknown-none riscv64gc-unknown-none-elf

# QEMU system emulators
#   Debian/Ubuntu: sudo apt install qemu-system-x86 qemu-system-arm qemu-system-misc
```

All builds are driven from the workspace root. The per-target linker scripts and
relocation model are wired in `.cargo/config.toml`, so a plain `cargo build`
with `--target` is all that is needed.

## Host verification (no hardware)

```sh
cargo test -p shared --features pqc-full,std     # foundation + real PQC
cargo test -p cibios --features test-crypto       # firmware logic + signed-image path
```

## x86_64

```sh
cargo build -p cibios --target x86_64-unknown-none --release
```

The binary carries a **multiboot1** header, so QEMU can load it with `-kernel`:

```sh
qemu-system-x86_64 \
  -kernel target/x86_64-unknown-none/release/cibios \
  -serial stdio -display none
```

Expected serial output: the `CIBIOS vX.Y.Z starting` banner, detected core/RAM
line, the assembled profile line, and `CIBIOS ready; awaiting CIBOS image
source`.

**Board values to confirm** (`cibios/linker/x86_64.ld`): load address `1M`
(standard multiboot), 64 KiB boot stack. The boot path enters in 32-bit
protected mode and trampolines to long mode (`cibios/src/bare/boot/x86_64.s`).

## AArch64 (QEMU `virt`, EL1)

```sh
cargo build -p cibios --target aarch64-unknown-none --release

qemu-system-aarch64 \
  -machine virt -cpu cortex-a72 \
  -kernel target/aarch64-unknown-none/release/cibios \
  -serial stdio -display none
```

QEMU passes the device-tree pointer in `x0`; CIBIOS parses it for CPU count and
memory. Serial is the PL011 at `0x0900_0000`.

**Board values to confirm** (`cibios/linker/aarch64.ld`): load address
`0x4008_0000`, and the PL011 base in `cibios/src/bare/arch/aarch64.rs`.

## RISC-V 64 (QEMU `virt`, S-mode under OpenSBI)

```sh
cargo build -p cibios --target riscv64gc-unknown-none-elf --release

qemu-system-riscv64 \
  -machine virt \
  -bios default \
  -kernel target/riscv64gc-unknown-none-elf/release/cibios \
  -serial stdio -display none
```

OpenSBI (the default `-bios`) runs in M-mode and jumps to CIBIOS in S-mode with
the hart id in `a0` and the device-tree pointer in `a1`. Console output uses the
SBI `console_putchar` call, so no UART address is hard-coded.

**Board values to confirm** (`cibios/linker/riscv64.ld`): load address
`0x8020_0000` (just above OpenSBI).

## What runs today vs. what is next

CIBIOS currently boots, initializes serial, detects hardware (CPUID on x86_64,
device tree on the others), assembles and validates a hardware profile, and
reports readiness. The verify → handoff → jump-to-kernel path is implemented and
host-tested (`boot_image` in `cibios/src/bare/mod.rs`); it activates once a CIBOS
image is provided to the firmware for the target — a multiboot module on x86_64,
or a fixed load address / storage driver on the others. That image-acquisition
step, and the CIBOS kernel it hands off to, are the next components.
