# DTB platform discovery + the hardcoded-value inventory

## Why
To stop "building around QEMU": platform specifics (RAM base, device addresses)
must come from runtime DATA (the firmware's device tree), not compiled-in QEMU
constants — so the same kernel boots on QEMU AND real boards without knowing
which.

## What landed
- cibos-dtb (NEW no_std crate, from-scratch, zero external crates,
  forbid(unsafe_code)): a flattened-device-tree (FDT/DTB) parser. Extracts the
  /memory reg (RAM base+size) and device bases. 4 tests INCLUDING one against a
  REAL captured QEMU virt DTB (not just synthetic) — proving it parses the
  real-world format, which is what matters for real hardware.
- Boot ABI: kernel_entry now takes (handoff_ptr, dtb_ptr). riscv64 boot.s
  preserves a1 (the DTB per the RISC-V boot protocol); aarch64 boot.s passes x0.
  The DTB pointer is stashed; synth_handoff reads the real RAM region from it via
  dtb_ram_region(), falling back to the conventional QEMU-virt base/size if no
  DTB is present or parseable.
- Result: the RAM layout is now DISCOVERED, not hardcoded, whenever a DTB is
  passed. Verified: the parser reads RAM base 0x40000000 from a real QEMU aarch64
  DTB. All arches still build clean; 374 tests pass; aarch64+riscv64 still boot
  with MMU online.

## Honest limitation found
QEMU's `-kernel <ELF>` path for aarch64/riscv64 does NOT pass a DTB pointer in the
entry register (it's 0), so in THAT test path the documented fallback is used. The
DTB MECHANISM is correct and proven against real DTB data; it engages whenever a
real pointer arrives — the CIBIOS firmware path (real hardware) and U-Boot/UEFI
provide one. So the hardcoded-constant problem is SOLVED for the real-hardware
path; the QEMU `-kernel` convenience path keeps a labeled fallback.

## Hardcoded-value INVENTORY (what is/ isn't platform-variable)
Platform-VARIABLE (must come from DTB on ARM/RISC-V) — now DTB-driven or flagged:
  - aarch64 RAM base (was 0x40000000) → from DTB, fallback documented.
  - riscv64 RAM base (was 0x80000000) → from DTB, fallback documented.
  - Peripheral bases (PL011 0x09000000, GIC 0x08000000; UART 0x10000000, PLIC
    0x0C000000, CLINT 0x02000000) → still constants in the arch backends; NEXT to
    move to DTB lookup (device_base) as each driver is generalized. FLAGGED.
Architecturally FIXED (correctly hardcoded — NOT QEMU-specific):
  - x86 VGA 0xB8000, COM1 0x3F8, PCI config 0xCF8/0xCFC, 8259 PIC, e1000 BAR enable
    bit 0x80000000 — these are fixed by the x86 PC architecture, identical on
    every PC. Leaving them as constants is correct.
  - Virtual addresses / IDs (ring3 entry rip, channel ids) — not platform addrs.

## NEXT for full dynamic platform discovery
1. Move the aarch64/riscv64 PERIPHERAL bases (UART/GIC/PLIC) to DTB device_base
   lookups as those drivers generalize (the parser already supports device_base).
2. Get a real DTB pointer on the QEMU test path too (load via -dtb / read QEMU's
   placed DTB) OR rely on the CIBIOS firmware path for real-hardware discovery.
3. Then no platform address is compiled in for ARM/RISC-V — true bare-metal-first.
