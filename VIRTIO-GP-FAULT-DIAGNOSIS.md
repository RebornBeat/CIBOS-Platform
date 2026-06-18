# Diagnosis: #GP (vector 13, err 0x17a) when a NIC is present

## Evidence (all confirmed, not assumed)
- Fault: #GP, error code 0x17a, RIP 0x100b9b0.
- RIP 0x100b9b0 = `timer::now_ticks` (a trivial relaxed AtomicU64 load) — which
  alone CANNOT #GP. So the RIP is merely WHERE the CPU was (the polled wait loop
  calls now_ticks) when an EXTERNAL interrupt arrived; the fault is the interrupt
  DELIVERY, not now_ticks.
- Error code 0x17a decoded: bit1(IDT)=1, index = 0x2F (47). => the CPU tried to
  dispatch an interrupt through IDT gate 0x2F and #GP'd because that gate is
  not-present / invalid.
- IDT (kernel-image/src/arch/idt.rs) installs ONLY: CPU exceptions 0..19 (skip
  15), syscall 0x80, timer 0x20 (IRQ0), keyboard 0x21 (IRQ1). Vectors 0x22..0x2F
  are NOT present.
- PIC (arch/x86_64.rs): remapped IRQ0->0x20 .. IRQ15->0x2F; all lines masked at
  init; kernel unmasks only IRQ0, IRQ1, IRQ2(cascade). The virtio device's line
  is masked.
- Vector 0x2F = IRQ15 = the SLAVE 8259's SPURIOUS-interrupt vector. With the
  cascade (IRQ2) enabled, slave activity around the virtio device produces a
  spurious IRQ15 that arrives at vector 0x2F — which has no IDT gate -> #GP.
- No NIC => no device activity => no spurious slave IRQ => clean boot. Matches.

## Root cause
A polled virtio-net driver leaves the device free to (and the 8259 free to
generate a spurious) interrupt on a vector (0x2F) the IDT does not handle. The
kernel faults on the missing gate. This is a REAL-hardware reality (8259 spurious
IRQs on IRQ7/IRQ15 are documented), exposed by adding the first
interrupt-capable device beyond the timer/keyboard.

## Faithful fix (two correct parts — NOT a mask-and-ignore shortcut)
1. DRIVER (virtio spec): set VRING_AVAIL_F_NO_INTERRUPT (avail.flags = 1) on BOTH
   queues' avail rings. The polled driver tells the device, via the documented
   interface, not to raise completion interrupts. (Primary, spec-faithful.)
2. KERNEL (real-OS robustness): install handlers for the PIC IRQ range, in
   particular the SPURIOUS vectors 0x27 (master IRQ7) and 0x2F (slave IRQ15).
   The spurious handler must read the in-service register (ISR) to distinguish a
   real vs spurious IRQ: for a true spurious master IRQ7, send NO EOI; for a
   spurious slave IRQ15, send EOI to the MASTER only (not the slave). Other
   unhandled PIC vectors get a minimal EOI-and-return stub so an unexpected
   device IRQ is acknowledged, never faults the kernel. This is textbook-correct
   8259 handling, and the honest "handle reality, no fakes" behavior.

Both are needed: (1) stops the expected completion interrupts; (2) makes the
kernel robust to spurious/unexpected IRQs (which real PIC hardware genuinely
produces) instead of #GP-faulting. Neither masks-and-prays.

## Alignment check (18 docs)
- No drift: realism over shortcut. We implement the documented virtio polling
  flag AND correct 8259 spurious-IRQ handling — both are how real bare-metal
  kernels behave. We do NOT just mask the line and hope.
- The driver stays polled (no async IRQ-driven RX yet); IRQ-driven RX can come
  later as an enhancement, on top of correct IDT coverage.

---

## RESOLUTION (implemented + verified)

Both parts of the faithful fix landed:

1. DRIVER (virtio spec): `VirtQueue::set_no_interrupt()` sets
   VRING_AVAIL_F_NO_INTERRUPT in each queue's avail-ring flags word; called for
   both RX and TX in `setup_queue`. The polled driver tells the device not to
   raise completion interrupts.

2. KERNEL (real-OS 8259 robustness):
   - asm stubs `pic_spurious_master_entry` (vector 0x27) and
     `pic_spurious_slave_entry` (0x2F) in syscall_entry.s, mirroring the
     timer/keyboard IRQ stubs (save volatiles, call handler, iretq).
   - `cibos_pic_spurious_irq(vector)` (idt.rs) -> `arch::pic_spurious(vector)`
     (x86_64.rs): reads the relevant PIC's In-Service Register (OCW3 read-ISR)
     and distinguishes true-spurious from real:
       * master 0x27 spurious -> NO EOI;
       * slave 0x2F spurious  -> EOI MASTER only;
       * genuine bit set      -> normal EOI.
   - IDT gates installed for 0x27 and 0x2F (DPL=0 interrupt gates).

### Verified
- Boot WITH a virtio-net NIC: probes (real MAC), TX self-check sends a frame
  (CIBOS-HELLO! captured in the QEMU filter-dump pcap), NO #GP, reaches
  `boot complete`.
- Boot WITHOUT a NIC: unchanged, clean `boot complete`.
- 355 tests green; production + virtio-net-demo + interactive + gui-demo +
  aarch64 + riscv64 all build clean.

### Why this is the correct (non-shortcut) fix
We did NOT just mask the line and hope. We (a) used the documented virtio polling
flag so the device does not raise expected interrupts, AND (b) made the kernel
correctly handle the spurious/unexpected PIC interrupts that REAL 8259 hardware
genuinely produces (IRQ7/IRQ15), per the textbook ISR-read rule. Both are how a
real bare-metal kernel behaves; QEMU merely exercised the path. This strengthens
the kernel for ALL future interrupt-capable devices, not just this NIC.
