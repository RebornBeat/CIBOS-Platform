# What "verified" actually means here — QEMU vs bare metal, per arch

An honest accounting, because the distinction matters and overclaiming would be
dishonest.

## The sandbox reality
- This sandbox is an x86_64 Linux host, NO KVM (pure emulation), QEMU 8.2.2.
- There is NO real aarch64 hardware, NO real riscv64 hardware, NO dual-boot, NO
  second physical machine. Every non-x86 test is QEMU EMULATING that ISA on an
  x86_64 host. Even x86_64 is emulated (TCG), not KVM-virtualized.
- So: I cannot run ANY of these on real silicon from here. Not x86_64 either.

## What that means for each claim — be precise
"Verified in QEMU" means: the code executed correctly against QEMU's MODEL of the
hardware. That is strong evidence but NOT identical to real silicon. The gap per
layer:

1. ISA / CPU semantics: QEMU implements the architecture spec faithfully for the
   instructions we use (paging, traps, FP, atomics). Risk on real HW: LOW for
   standard instructions; QEMU is a widely-trusted reference. This is why the
   page-table encoders, the MMU enable, the trap vectors, etc. are GENUINELY
   meaningful — they target the ARCHITECTURE, not QEMU.

2. Platform memory map / devices: HIGHER risk. QEMU 'virt' places RAM at 1 GiB,
   PL011 at 0x09000000, GIC at 0x08000000 — these are QEMU-virt choices. A real
   ARM board (RPi, a server) has DIFFERENT addresses. On real hardware these come
   from the DEVICE TREE (DTB) / ACPI, which the firmware passes — they must NOT be
   hardcoded. RIGHT NOW the aarch64 RAM base (0x40000000) and peripheral
   addresses ARE effectively hardcoded for QEMU virt (via the synth handoff and
   the paging hooks). THIS IS THE REAL "building around QEMU" RISK, and it is
   honest to call it out: on a different board these constants are wrong.

3. Timing / peripherals / interrupts / errata: QEMU does not model real timing,
   cache behavior, or silicon errata. Drivers that pass in QEMU can still fail on
   real HW. Only real-hardware testing closes this.

## So how do we KEEP it bare-metal-correct without real silicon?
The discipline is: target the ARCHITECTURE and take platform specifics from
firmware DATA, never from compiled-in QEMU assumptions. Concretely:
  - CPU/MMU/trap code targets the ISA spec → QEMU verification is meaningful and
    transfers to real silicon (low risk).
  - Platform specifics (RAM base, device addresses, IRQ numbers) MUST come from
    the handoff/DTB at runtime, NOT constants. Where they are constants today
    (aarch64 RAM base, peripheral addrs), that is a KNOWN GAP to close: parse the
    DTB QEMU passes in x0 (we currently ignore it) — the SAME DTB a real board's
    firmware passes — so the kernel reads the real layout and works on BOTH QEMU
    and real hardware without knowing which it is. THAT is the bare-metal-first
    guarantee: the kernel never knows it's QEMU because it reads the platform
    from data, not from #[cfg(qemu)].
  - Final closure is real hardware. That can only happen OUTSIDE this sandbox: an
    x86_64 box from a USB image (the CIBIOS firmware path already targets real
    BIOS), an ARM board (RPi/UEFI) fed the kernel, a RISC-V board (VisionFive,
    etc.) via OpenSBI. Those tests are deferred and must be labeled as NOT YET
    DONE — claiming otherwise would be false.

## Per-arch honest status (verification level)
| Arch    | Builds | QEMU boots | Real HW | Platform addrs from firmware/DTB? |
|---------|--------|-----------|---------|-----------------------------------|
| x86_64  | yes    | full stack| NOT TESTED (firmware path targets real BIOS, untested on metal) | mostly (handoff); some PC constants |
| aarch64 | yes    | MMU online| NOT TESTED | NO — RAM base + peripheral addrs hardcoded for QEMU virt (GAP) |
| riscv64 | yes    | core boots| NOT TESTED | NO — same class of gap |
| i686    | yes    | firmware-path only | NOT TESTED | n/a (stub) |

## Conclusion (what I can and cannot claim)
I CAN claim: the code builds and runs correctly against QEMU's faithful model of
each ISA; the architecture-level work (encoders, MMU, traps) is real and largely
transfers. I CANNOT claim: it runs on real aarch64/riscv64 silicon, or that the
platform-specific constants are correct for any board other than QEMU virt. The
path to a true bare-metal guarantee is: (1) read platform layout from the DTB/
handoff at runtime instead of constants, then (2) test on real hardware outside
this sandbox. Both are explicitly OUTSTANDING, not done.
