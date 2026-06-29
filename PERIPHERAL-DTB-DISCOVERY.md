# Moving peripheral bases from hardcoded constants to DTB discovery

## Scope (from the hardcoded-value inventory)
RAM base is already DTB-driven. This step moves the PERIPHERAL bases off the
compiled-in QEMU-virt constants:
  - aarch64: PL011 UART at 0x09000000 (the one genuine hardcode; used by putc).
  - riscv64: the kernel uses OpenSBI (SBI calls) for console, so NO UART MMIO
    address is hardcoded — nothing to move there for the console. (PLIC/CLINT are
    not yet driven by the kernel; when an interrupt-controller driver is added it
    will read its base from the DTB the same way.)
  - GIC (aarch64 intc@8000000): not yet driven by a kernel GIC driver; flagged
    for when that driver lands.

## The ordering constraint (why a naive move would corrupt boot)
The aarch64 UART is used by the VERY FIRST kprintln (init_serial -> "entry"),
which runs BEFORE the DTB is parsed. You cannot read the DTB to find the UART
before you have a UART to report DTB-parse errors on. So the correct pattern is
two-stage (this is exactly what real kernels do with "earlycon"):
  1. EARLY: a compiled-in bootstrap default (0x09000000) lets the kernel print
     during the earliest boot.
  2. AFTER DTB PARSE: update the UART base from the DTB's pl011 node
     (device_base(b"pl011")); subsequent output + real-hardware boards use the
     discovered address. If the DTB is absent/unparseable, the bootstrap default
     stays (correct for QEMU virt).

## Implementation
- aarch64.rs: UART0 becomes a runtime AtomicUsize initialized to the QEMU-virt
  default 0x09000000; putc reads it. A setter `set_uart_base(addr)` updates it.
- boot.rs: right after stashing DTB_PTR and before heavy output, parse the DTB
  (already have dtb_ram_region's machinery) and, if a pl011 base is found, call
  arch::set_uart_base. The node name in the real QEMU DTB is `pl011@9000000`, so
  the prefix b"pl011" matches.
- The DTB real-data test already proves device_base works on the real format.

## Honesty
On QEMU `-kernel` the DTB pointer is 0 (QEMU doesn't pass it that way), so the
bootstrap default is what's used there — correct for QEMU virt. The DISCOVERY
engages whenever a real DTB pointer arrives (CIBIOS firmware / U-Boot / UEFI),
which is the real-hardware path. No address is silently wrong: the default is the
QEMU-virt value and the override is DTB-derived.

---

## DONE — verified
- aarch64.rs: UART0 is now an AtomicUsize defaulting to the QEMU-virt 0x09000000
  (earlycon), with set_uart_base() to override from the DTB. putc reads the atomic.
- boot.rs: dtb_device_base(prefix) helper (aarch64-scoped); kernel_entry calls
  arch::set_uart_base(dtb_device_base(b"pl011")) right after stashing the DTB, so
  later output + real boards use the discovered UART address. Early "entry" line
  still prints via the bootstrap default.
- cibos-dtb: new real-DTB test finds_pl011_uart_base proves device_base(b"pl011")
  resolves to 0x09000000 in the REAL captured QEMU DTB (the node is pl011@9000000).
- Verified: all 3 arches build clean; x86_64 full stack UNCHANGED (MMU online,
  STACK OK, REMOTE LINK OK, boot complete); aarch64 + riscv64 boot, MMU online;
  375 tests pass (+1).
- riscv64: console uses OpenSBI (SBI calls), so no UART MMIO to discover — correct
  to leave as-is until a PLIC/CLINT driver lands (which will read its base from the
  DTB the same way).

## Remaining peripheral work (when the drivers land)
- aarch64 GIC (intc@8000000): read base from DTB when a kernel GIC driver is added.
- riscv64 PLIC/CLINT: read bases from DTB when those drivers are added.
These are NOT hardcoded-and-used today (no kernel driver touches them yet), so
there is nothing currently wrong to fix — they are flagged for when built.

---

## Correctness review (dwelling on this work)

Re-examined the UART-from-DTB change for hazards beyond "it builds":

1. VERIFIED CORRECT for what we can test: on aarch64 QEMU, "boot complete" prints
   AFTER "MMU online", proving putc writes to the UART base THROUGH the kernel's
   own page tables — i.e. the UART address is correctly identity-mapped and
   survives the MMU switch. The two-stage (earlycon default -> DTB override)
   logic works; the atomic load in putc is on the hot path but Relaxed ordering is
   fine (single value, no dependent memory).

2. KNOWN REAL-HARDWARE EDGE CASE (documented, not speculatively coded): the
   override sets the UART base early, but the MMU identity map (0..1.25 GiB) is
   built later. If a REAL board's DTB reports a UART base ABOVE 1.25 GiB, putc
   would fault after the MMU comes online (the address would be unmapped). On QEMU
   virt the UART is at 0x09000000 (well inside the map), so there is no bug today.
   The robust fix when targeting such a board: have the MMU phase add the
   DTB-discovered peripheral bases to mmio_identity_ranges() (the hook already
   exists; aarch64 returns empty only because QEMU's peripherals are within the
   low map). NOT done now because: (a) no real hardware to test it on, and (b)
   adding untestable speculative mapping risks a bug we cannot verify — which would
   violate "don't corrupt anything". Flagged here so it is addressed when a real
   board with a high UART is actually in hand.

3. No corruption introduced: x86_64 and riscv64 paths are untouched by this change
   (riscv64 console is SBI, x86 is port-I/O COM1); only aarch64 putc reads the
   atomic. All 375 tests pass; all three arches boot.
