# Supported architectures

CIBOS firmware (CIBIOS) and kernel build for four CPU architectures. The first
three use Rust's stable precompiled bare-metal targets; 32-bit x86 uses a custom
target spec with `-Z build-std` on nightly (it has no stable bare target).

| Arch | `ProcessorArchitecture` | Target | Toolchain | Boot |
|---|---|---|---|---|
| x86-64 | `X86_64 = 1` | `x86_64-unknown-none` | stable | multiboot1, 32→64 long-mode switch |
| AArch64 | `AArch64 = 2` | `aarch64-unknown-none` | stable | DTB, `x0` handoff |
| 32-bit x86 | `X86 = 3` | `targets/i686-cibos-none.json` | nightly + build-std | multiboot1, stays in 32-bit PM |
| RISC-V 64 | `RiscV64 = 4` | `riscv64gc-unknown-none-elf` | stable | SBI, `a0` handoff |

## Why 32-bit x86 for old hardware

CIBOS is meant to run on any device, however old. Many legacy machines are
32-bit-only with a legacy BIOS. The 32-bit path is intentionally the *simplest*
boot of the four: a multiboot1 loader drops the kernel into 32-bit protected
mode at 1 MiB, which is already the environment a 32-bit kernel runs in — no
mode switch, no page-table setup before handoff. Entropy uses `RDTSC` jitter
(present since the Pentium), with `RDRAND` only when CPUID advertises it.

## Building

Stable targets:

```sh
cargo build -p cibios --target x86_64-unknown-none
cargo build -p cibios --target aarch64-unknown-none
cargo build -p cibios --target riscv64gc-unknown-none-elf
```

32-bit x86 (nightly + build-std, wrapped in a script):

```sh
./build-i686.sh
# -> target/i686-cibos-none/debug/cibios  (ELF 32-bit LSB, Intel 80386)
```
