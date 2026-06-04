# Booting the full chain: CIBIOS → CIBOS

This boots the real firmware-to-kernel handoff: QEMU loads **CIBIOS** (the
firmware) as the kernel and the **CIBOS image** (`cibos-<arch>.cimg`, containing
the kernel) as a boot module. CIBIOS detects the hardware, locates the image
module, verifies it, copies the kernel component to its load address, builds the
handoff record, and jumps to the kernel — which then brings up the heap and the
scheduler.

This is distinct from the kernel's standalone `self-boot` path (see
`kernel-image/QEMU.md`), where the kernel boots alone with a synthesized
handoff. Here the handoff is real, produced by the firmware from detected
hardware.

## Build both artifacts

```sh
# one-shot: builds all kernel images, verifies them, builds the firmware
./mkimages.sh
```

This produces:

* `target/x86_64-unknown-none/debug/cibios` — the firmware ELF
* `images/cibos-x86_64.cimg` — the CIBOS image (kernel component)
* (and the aarch64 / riscv64 `.cimg` images)

The images are **Lightweight** (unsigned). Each is verified through CIBIOS's own
`ImageView::parse` + `verify_image` before you ever boot it, so a malformed or
corrupt image is caught at build time.

## Boot (x86_64)

CIBIOS is a multiboot1 kernel; QEMU passes `-initrd` to it as the first boot
module, which is exactly where CIBIOS's `locate_image` looks.

```sh
qemu-system-x86_64 \
  -kernel target/x86_64-unknown-none/debug/cibios \
  -initrd images/cibos-x86_64.cimg \
  -serial stdio -display none
```

## Expected serial transcript

```
CIBIOS v0.1.0 starting
detected: N core(s), M MiB RAM at 0x...
profile: X86_64 on Desktop, ... SMT off
firmware profile: Lightweight
CIBOS image found (405936 bytes); booting
image verified (signature skipped), entry 0x1000000
components placed
handoff built; transferring control to CIBOS
CIBOS kernel: entry
CIBOS kernel: heap online (8388608 bytes)
CIBOS kernel: handoff accepted, ... bytes usable across 1 region(s)
CIBOS kernel: init lane running
CIBOS kernel: scheduler idle after 1 poll(s)
CIBOS kernel: boot complete
CIBOS kernel: halt
```

The transition from the `CIBIOS …` lines to the `CIBOS kernel: …` lines is the
handoff crossing the firmware/kernel boundary at runtime. `components placed`
then `handoff accepted` is the chain closing: the firmware staged the verified
kernel and the kernel accepted the firmware's handoff record.

Stop QEMU with `Ctrl-A x`.

## Boot (aarch64 / riscv64)

On the QEMU `virt` machines there is no multiboot. Instead QEMU loads `-initrd`
into RAM and publishes its location in the device tree under
`/chosen/linux,initrd-start` and `linux,initrd-end`. CIBIOS reads the DTB
pointer the platform passes at entry (x0 on aarch64, a1 on riscv64), parses
`/chosen`, and finds the image — the same `boot_image` path as x86_64 from there.

AArch64:

```sh
qemu-system-aarch64 -machine virt -cpu cortex-a53 -m 512M \
  -kernel target/aarch64-unknown-none/debug/cibios \
  -initrd images/cibos-aarch64.cimg \
  -serial stdio -display none
```

RISC-V 64:

```sh
qemu-system-riscv64 -machine virt -m 512M \
  -kernel target/riscv64gc-unknown-none-elf/debug/cibios \
  -initrd images/cibos-riscv64.cimg \
  -serial stdio -display none
```

The serial transcript matches the x86_64 one: the `CIBIOS …` lines hand off to
the `CIBOS kernel: …` lines once the image is located, verified, and placed.

To build the firmware for these targets:

```sh
cargo build -p cibios --target aarch64-unknown-none
cargo build -p cibios --target riscv64gc-unknown-none-elf
```

## How the image is made

`mkimage` wraps a flat kernel binary into the CIBOS image format using the
firmware's own build module, so the producer and consumer share one definition:

```sh
# flatten the kernel ELF to a raw load image
llvm-objcopy -O binary target/x86_64-unknown-none/debug/cibos-kernel kernel.bin
# wrap it (arch, entry, load address)
mkimage build x86_64 0x1000000 0x1000000 kernel.bin cibos-x86_64.cimg
# verify through the firmware parser/verifier
mkimage verify cibos-x86_64.cimg x86_64
```
