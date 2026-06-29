# Static vs dynamic device discovery — the principle (resolving the back-and-forth)

## The legitimate question
"At first we used baselines, then switched to full dynamic, now we're doing static
for VGA and others. Are these the required properties to read anything at all? Is
that universal on any board? Are we keeping static only for the irreducible
bootstrap while everything else stays dynamic?"

## The answer: there are THREE tiers, not two
Device addresses fall into three categories, and conflating them caused the
apparent back-and-forth:

### Tier 1 — IRREDUCIBLE BOOTSTRAP (must be static / compiled-in or bootloader-passed)
To read ANY dynamic platform description, the kernel must already be able to (a)
emit output and (b) find the platform tables. These cannot themselves be
discovered dynamically — it is the bootstrap paradox. Every real OS (Linux, BSD,
EDK2, U-Boot) has this minimum:
  - An EARLYCON: one console address known before discovery, used only until the
    real console is found. (We have this: the aarch64 bootstrap UART default, used
    only until the DTB pl011 base is read.)
  - A HANDOFF: the DTB pointer in a register (ARM/RISC-V) or the ACPI RSDP scan
    range / E820 (x86), provided by firmware. (We have this: kernel_entry's dtb_ptr
    + the firmware HandoffData.)

### Tier 2 — ARCHITECTURAL CONSTANTS (legitimately static, universal per arch)
Addresses FIXED BY THE ARCHITECTURE, identical on every machine of that family —
not a board or emulator choice:
  - x86/PC: VGA text buffer 0xB8000 (PC/VGA standard since 1987), COM1 port 0x3F8,
    the legacy port-I/O map. These are TRUE on every IBM-PC-compatible, real or
    emulated. Static is CORRECT and universal — discovering them would be pointless
    (they cannot vary).
  - These are part of "what you need to read anything on a PC" and are genuinely
    architectural, so they stay static WITHOUT being a shortcut.

### Tier 3 — BOARD-SPECIFIC (MUST be dynamic; static here IS a QEMU-era shortcut)
Addresses that vary per board/SoC/chipset:
  - aarch64: GIC, the actual UART base (Pi vs Graviton vs NXP all differ).
  - riscv64: CLINT, PLIC, UART (SiFive vs StarFive differ).
  - x86: the PCI MMIO hole / device BARs (chipset- and RAM-size-dependent).
  These MUST come from the DTB (ARM/RISC-V) or PCI BAR enumeration (x86). Hardcoding
  them to the QEMU-virt / i440fx layout is the shortcut: right on QEMU, WRONG on
  real boards.

## So, resolving the concern precisely
- KEEP static: Tier 1 (bootstrap earlycon + handoff mechanism) and Tier 2
  (architectural constants like PC VGA/COM1). These are required and universal;
  they are NOT the QEMU shortcut.
- MAKE DYNAMIC: Tier 3 (GIC/PLIC/CLINT/board UART via DTB; PCI hole via BAR
  enumeration). This is where static = shortcut, and where the work goes.

The discovery MECHANISM already exists end-to-end: the mmio_registry + the unified
carve/Device-map consume whatever (base,len) ranges they are handed, with no cap
and loud failure. The only missing piece is the PRODUCER: parse the DTB's
interrupt-controller / timer / uart nodes and register THOSE, instead of the
static Tier-3 arrays. Then the static arrays shrink to Tier 2 only.

## Plan
1. Extend cibos-dtb to resolve device nodes by compatible/type: GIC (arm,gic-*),
   PLIC (riscv,plic0), CLINT (riscv,clint0/sifive,clint), and the chosen UART.
2. At boot, register the DISCOVERED Tier-3 ranges into mmio_registry (the carve/
   Device flow already maps them). Remove the Tier-3 entries from
   mmio_identity_ranges, leaving ONLY Tier-2 architectural constants there.
3. Fallback: if the DTB lacks a node (or no DTB, e.g. QEMU -kernel), fall back to
   the known QEMU-virt address AS A LABELED EARLYCON-STYLE DEFAULT — explicitly a
   bootstrap fallback, not the primary path (same pattern already used for the
   UART). On real hardware with a real DTB, the discovered values win.
4. x86 PCI hole: enumerate the device BARs (we already touch PCI config for the
   NIC) and register the actual BAR window instead of the i440fx constant.

---

## DONE — Tier-3 board-specific devices made DTB-dynamic (ARM + RISC-V)
Resolving the actual back-and-forth concern: the board-specific device addresses
are no longer static. Implemented:
  - cibos-dtb gained device_reg(prefix) -> (base, size), reading a node's reg
    window from the DTB (mirrors ram_region).
  - aarch64: GIC discovered via dtb_device_reg(b"intc"); UART already dynamic. The
    static mmio_identity_ranges is now EMPTY.
  - riscv64: PLIC (plic@), CLINT (clint@), UART (serial@) discovered via
    dtb_device_reg. The static mmio_identity_ranges is now EMPTY.
  - Each has a LABELED bootstrap fallback to the QEMU-virt address used ONLY when
    no DTB is present (QEMU -kernel), registered the same dynamic way — not a
    primary path. On real hardware with a real DTB, the discovered values win.
  - The discovered windows flow into the existing mmio_registry -> carve -> Device
    map (no cap, loud fail), so the address SOURCE is now the platform.

VERIFIED:
  - Real-DTB unit tests (captured QEMU DTBs): device_reg(b"intc")=0x08000000 (ARM);
    plic=0x0C000000, clint=0x02000000, serial=0x10000000 (RISC-V). Proves discovery
    works on real platform data, not just the fallback. 380 tests pass.
  - All 3 boot (aarch64/riscv64 MMU online + boot complete; x86 full stack); UART
    still works on aarch64 via the dynamic/fallback registration.

## What remains static — and why that's correct (Tier 1 + Tier 2 only)
  - Tier 1 bootstrap: the earlycon UART default + the firmware/DTB handoff
    mechanism. Irreducible — required to read anything. Universal.
  - Tier 2 architectural constants: x86 VGA 0xB8000, COM1 0x3F8 — fixed by the
    PC/VGA architecture on every compatible machine, real or emulated. Static is
    correct and not a shortcut.
  - x86 PCI hole (0xFEB00000) remains a static Tier-3 item — the honest next step
    is to enumerate the actual device BARs from PCI config space (we already touch
    PCI for the NIC) and register THAT window, replacing the i440fx constant. The
    ARM/RISC-V pattern (discover -> register -> carve/Device) is the template.

---

## ALL FALLBACKS REMOVED — no QEMU fallback anywhere
Per the directive "remove all fallbacks; if it works without the fallback, the
fallback is wrong design and can hide bugs." Measured first (don't assume):
  - riscv64: QEMU passes a REAL DTB via OpenSBI (probed dtb_ptr = 0x87e00000). So
    discovery already used the real path; the fallback was dead. REMOVED.
  - aarch64: QEMU `-kernel <ELF>` passed x0 = 0 (no DTB) — the ELF lacked the ARM64
    image header, so QEMU did not treat it as a Linux image. FIXED PROPERLY by
    adding the standard ARM64 image header to the kernel and booting it as a raw
    Image (build-arm64-image.sh -> objcopy -O binary). A conforming loader (real
    U-Boot/UEFI AND QEMU) then passes the DTB in x0. Probed: dtb_ptr = 0x44000000,
    RAM from DTB. So the fallback became unnecessary. REMOVED.

What was removed:
  - aarch64 UART + GIC device fallbacks (now DTB-only; warn if a node is absent).
  - riscv64 PLIC/CLINT/UART device fallbacks (now DTB-only; warn if absent).
  - aarch64 + riscv64 RAM-region fallback in synth_handoff (now panics loudly if
    the DTB has no usable RAM node — a real platform fault, not something to limp
    past on a QEMU-virt constant).
  - the unused uart_base() getter (only the fallback used it).

What legitimately remains (NOT fallbacks):
  - Tier-1 earlycon: aarch64 UART0 default 0x09000000, used ONLY to print before
    the DTB pl011 base is read, then overridden. The irreducible bootstrap console.
  - Tier-2 architectural constants: x86 VGA 0xB8000, COM1 0x3F8 (PC standard).
  - x86 self-boot synth RAM (0x100000/128 MiB): x86 has NO DTB; this is the x86
    self-boot bootstrap (real x86 uses the CIBIOS firmware handoff). Not a QEMU
    device fallback.

Why this is correct for bare metal: a fallback to QEMU-virt addresses implies "no
DTB" is a supported real scenario (it is not — real boards always provide a DTB via
firmware/U-Boot, or go through CIBIOS), and it silently masks a DTB-parsing bug by
limping on hardcoded values. Removing it means a DTB problem FAILS LOUDLY, and the
QEMU path exercises the SAME real-DTB code path as hardware.

VERIFIED: 380 tests pass; all 3 build clean; all 3 boot with NO fallbacks (aarch64
via the ARM64 Image reading the real DTB; riscv64 via OpenSBI's DTB; x86 full stack
via the CIBIOS/self-boot handoff). The ARM64 image header also makes aarch64 boot
the SAME way on real hardware as in QEMU — a strict improvement in real-HW fidelity.
