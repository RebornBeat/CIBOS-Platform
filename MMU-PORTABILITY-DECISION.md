# bring_up_mmu — portable orchestration vs per-arch reimplementation

## The question
Earlier I said a "naive trait split would break the data flow" because
bring_up_mmu creates frames/space and then NIC-probe + ring-3 borrow them inside
its scope. The question: rather than reimplement bring_up_mmu per arch (lots of
duplication, drift-prone), can we split it NEATLY so all arches share it?

## The finding (decisive)
The coupling I flagged is ORCHESTRATION coupling, NOT arch coupling. Reading
bring_up_mmu line by line, the arch-specific surface is tiny and ALREADY
abstracted in cibos_kernel::paging:
  - cibos_kernel::paging::PageTableEncoder trait = 4 methods (encode_table,
    encode_leaf, is_present, entry_frame).
  - AddressSpace::map_range::<E: PageTableEncoder> is FULLY GENERIC over the
    encoder — all page-table walking / frame alloc / mapping is portable.
  - Per arch, the backend supplies: the 4-method encoder + 4 register fns
    (install root, enable-NX-equivalent, current_root, current_root_frame).
Everything else in bring_up_mmu — collect regions, build the frame allocator,
identity-map logic, install, verify isolation, probe NIC, enter ring-3 — is
PORTABLE orchestration. It is just not WRITTEN generically: it hardcodes
X86PageTable and inlines x86 register calls.

## Decision: generalize the ONE orchestration; per arch supply only the hooks
Do NOT reimplement bring_up_mmu per arch (that is the redundancy/drift risk).
Instead make the single MMU bring-up generic over an arch "paging hooks" surface,
so:
  - ONE bring_up_mmu orchestration, correct on every arch.
  - Each arch adds ~8 small fns (4 encoder + 4 register), NOT ~150 lines of
    duplicated orchestration.
  - The identity-map PARAMETERS that differ per arch (which MMIO ranges: x86 PCI
    hole 0xFEB..; aarch64 GIC/PL011; riscv64 PLIC/UART) are data the arch hook
    supplies, not forked code.
This is MORE work up front than a per-arch copy, but it is LESS total work and
MORE accurate: a bug fixed in the orchestration is fixed for all arches at once,
and no arch can drift because they share the one code path. This is the same
principle as the ArchBringUp contract, applied one level deeper.

## The arch paging-hooks surface (what each backend provides)
A trait (in kernel-image, e.g. ArchPaging) bundling what bring_up_mmu needs:
  - type Encoder: PageTableEncoder            // the 4-method entry format
  - unsafe fn enable_table_features()         // x86: EFER.NXE; arm/riscv: their setup
  - unsafe fn install(root: PhysFrame)        // write TTBR/satp/CR3
  - fn current_root() -> u64
  - fn mmio_identity_ranges() -> &[(base,len)] // device MMIO to map (PCI hole / GIC / PLIC)
x86_64 impl wires the existing X86PageTable + install + enable_nxe + current_root
+ the PCI MMIO hole — NO behavior change. aarch64/riscv64 implement the same
surface with their encoders (ARM VMSAv8-64 4KB descriptors; RISC-V Sv39 PTEs) and
register ops (TTBR0_EL1/TCR_EL1/SCTLR_EL1.M; satp) — then the SHARED bring_up_mmu
runs unchanged on them.

## Sequencing
1. Define ArchPaging hooks; refactor bring_up_mmu to be generic over them.
2. x86_64 impl = current behavior (VERIFY identical boot).
3. aarch64 encoder (VMSAv8-64, 4KB granule) + register ops + MMIO ranges; the
   shared orchestration brings the MMU online on aarch64 (verify: "MMU online").
4. riscv64 encoder (Sv39) + satp + ranges; shared orchestration on riscv64.
5. Then storage / NIC (virtio-mmio) / ring-3 per arch — each likewise generalized
   where the logic is portable (virtqueue logic is already shared), arch hook
   where it is not.

---

## RESULT — MMU online on THREE arches via the ONE shared orchestration

bring_up_mmu_generic<P: ArchPaging> now brings the MMU online on x86_64, aarch64,
AND riscv64 — each supplying only its encoder + register ops + DTB-derived
parameters, NOT a duplicated orchestration:
  - x86_64: X86PageTable (4-level), CR3 install, EFER.NXE, PCI MMIO hole.
    UNCHANGED — full stack still verified (MMU online + STACK OK + REMOTE LINK OK).
  - aarch64: Aarch64PageTable (VMSAv8-64, 4KB granule), TTBR0_EL1/TCR/MAIR/SCTLR
    install. "MMU online — root 0x42000000", boot complete.
  - riscv64: Sv48PageTable (4-level — Sv48 matches the portable geometry; Sv39 is
    3-level and would NOT), satp install. "MMU online — root 0x82000000", boot
    complete. Worked FIRST TRY (the orchestration was already correct from the
    aarch64 bring-up) — evidence the abstraction is right.

ArchPaging hooks (the entire per-arch surface): type Encoder, identity_map_bytes,
reserved_below, mmio_identity_ranges, enable_table_features, install,
current_root. ~7 small items per arch vs ~150 lines of duplicated orchestration.

Key lesson captured: the arch differences that surfaced were all DATA, not code —
RAM base (x86 1 MiB / aarch64 1 GiB / riscv64 2 GiB, READ FROM EACH DTB, not
guessed), reservation watermark, page-table geometry (use Sv48 not Sv39). This is
the no-drift design proving itself: one code path, per-arch data.

## IMPORTANT correctness caveat (see VERIFICATION-REALITY-AND-BARE-METAL.md)
The RAM base + peripheral addresses are currently QEMU-virt constants in the synth
handoff + paging hooks. They are CORRECT for QEMU virt (verified against each
board's DTB) but WRONG for other real boards. The bare-metal-correct fix is to
parse the DTB the firmware passes (in x0/a1) at runtime, so the kernel reads the
real layout and works on QEMU AND real hardware without knowing which. That DTB
parsing is the next correctness step before any real-hardware claim. All current
"verified" claims mean "against QEMU's faithful ISA model", NOT real silicon —
there is no real aarch64/riscv64 hardware in this sandbox.
