# i686 (32-bit x86) — complete completion roadmap (true 4/4)

Purpose: capture EVERY insight from the MMU-generalization arc so that bringing
i686 to full parity is a deliberate, no-shortcut, no-drift effort. This builds on
I686-PARITY-ACCOUNTING.md (the why) with the concrete, ordered HOW, and folds in
how each capability we built for the other 3 arches must be realized for i686.

================================================================================
## 0. Current i686 state (confirmed, this audit)
- Firmware (cibios, ELF32 i386) builds. Kernel image links with the corrected
  2-arg kernel_entry boot.s.
- ArchBringUp: early_traps (serial live, NO IDT yet), seed_entropy = DONE (routes
  to seed_entropy_portable, identical to all arches, full 32-byte seed),
  mount_root_fs / bring_up_mmu / verify_storage = Skipped (honest).
- NO PageTableEncoder impl, NO MMU bring-up. This is the one real architectural
  gap. Everything portable (heap, handoff, scheduler, entropy, RNG) already works
  because it has zero arch code.

================================================================================
## 1. The core blocker: i686 paging geometry ≠ the portable 4-level model
The portable layer (cibos_kernel::paging) is hardwired to:
  LEVELS = 4, INDEX_BITS = 9 (9 bits/level), 8-byte (u64) entries, 4 KiB pages.
This matches x86_64 / aarch64(4KiB granule) / riscv64(Sv48). i686 does NOT:
  - CLASSIC 32-bit: 2-level (10+10+12), 4-BYTE (u32) entries; large page = 4 MiB
    (PSE, PS bit in the PDE). Different level count AND entry width.
  - PAE: 3-level (2+9+9+12), 8-BYTE entries; large page = 2 MiB (PS bit in PDE);
    supports NX (bit 63) — needed for our W^X kernel/user perms. 3 levels, 2-bit
    top index.
DECISION (carry forward): use PAE. Rationale:
  * 8-byte entries match the existing u64 PageTableEncoder return type.
  * 2 MiB large page matches the 2 MiB block machinery we already built
    (map_block_2m, is_block_leaf at "block level").
  * NX support => our Permissions.execute (W^X) maps directly, same as x86_64.
  * Classic 2-level/4-byte would force a u32 entry path and 4 MiB blocks — more
    divergence, no upside.

================================================================================
## 2. Generalize the portable paging over geometry (the enabling refactor)
Today LEVELS/INDEX_BITS are crate consts and entries are u64. To host PAE (3
levels) without breaking the 3 working arches, make geometry per-encoder:
  - Add associated consts to PageTableEncoder:
        const LEVELS: usize;          // 4 for x86_64/aarch64/riscv64; 3 for PAE
        const TOP_INDEX_BITS: usize;  // 9 normally; 2 for PAE top level
        (entry width stays u64 — PAE entries are 8 bytes, so no u32 path needed.)
  - AddressSpace::indices() and the map/map_range/map_block_2m/translate walkers
    consume E::LEVELS and the per-level index width instead of the crate consts.
  - The block level becomes E::LEVELS-2 (PAE: level 1 = the PD, 2 MiB block) — the
    encode_block_leaf(frame, perms, LEVEL) signature ALREADY takes the level, so
    it is forward-compatible; no signature change.
  CRITICAL DISCIPLINE: this refactor MUST keep x86_64/aarch64/riscv64 byte-
  identical. Do it in steps, re-running the full 378-test suite + all 3 boots
  after EACH step. The geometry consts for those arches stay (4, 9), so behavior
  is unchanged by construction; the risk is mechanical (walker loops), so test
  continuously. Roll back instantly on any regression.

================================================================================
## 3. The i686 PAE encoder (arch-local, mirrors x86_64 exactly where it can)
New: kernel-image/src/arch/paging_i686.rs, `struct PaePageTable`, impl
PageTableEncoder with LEVELS=3:
  - encode_table: PDPTE/PDE pointing to next table; Present + (RW/US permissive,
    leaf authoritative), same philosophy as x86_64.
  - encode_leaf (4 KiB at level 2 = PT): Present, RW from perms.write, US from
    perms.user, NX(bit63) from !perms.execute. DEVICE MEMORY: set PWT(bit3)|
    PCD(bit4) when perms.device — IDENTICAL to x86_64. (This is why device memory
    "just works" for i686 — the field already flows; the encoder honors it the
    same way x86_64 does.)
  - encode_block_leaf (2 MiB at level 1 = PD): same bits as encode_leaf PLUS
    PS(bit7); frame 2 MiB-aligned. Device honored identically (PWT|PCD).
  - is_present: Present bit. is_block_leaf(entry, level): Present && PS set (same
    as x86_64). entry_frame: mask to the 8-byte entry's PPN.
  NOTE: PAE physical addresses are up to 52 bits in 8-byte entries; the ADDR_MASK
  differs from classic 32-bit. Use the PAE mask, not x86_64's.

================================================================================
## 4. i686 ArchPaging impl + wiring bring_up_mmu
- impl ArchPaging for the i686 Arch: Encoder = PaePageTable; kernel_span()
  (image+heap+stack above RAM base, like x86_64's 64 MiB); min_identity_map_bytes()
  (a floor covering low devices — VGA 0xB8000, and the PCI hole if mapping high);
  mmio_identity_ranges() (the i686 device windows — see §5); install(root) =
  load CR3 with the PDPT base AND set CR4.PAE (bit5) + EFER.NXE (for NX) BEFORE
  enabling paging; current_root() reads CR3.
- bring_up_mmu: route to the SAME shared bring_up_mmu_generic<PaePageTable>. No
  i686-special orchestration — the whole point of the generalization is that i686
  uses the identical derived-geometry, carve-out, Device-mapping, large-page flow
  as the other arches. Skipped -> Done.
  CR4.PAE + paging-enable sequence is the one delicate asm step: set CR4.PAE,
  load CR3, set EFER.NXE, then set CR0.PG. Order matters; get it from the SDM, do
  not guess.

================================================================================
## 5. How EACH arc capability realizes for i686 (no capability left behind)
- Derived geometry (no hardcoded QEMU size): i686 gets ram_base/ram_end from the
  REAL handoff (CIBIOS firmware on real HW). i686 has NO DTB; its platform info
  comes from the BIOS/firmware handoff (and E820 for the memory map). So the
  dtb_* paths stay aarch64/riscv64-only; i686 mirrors x86_64's handoff path.
- Large pages (2 MiB blocks): realized via PAE PS-bit blocks at the PD level. The
  map_block_2m/translate block-aware logic is generic over E::LEVELS, so it works
  for i686 once §2 lands. Same 512x page-table savings on big-RAM 32-bit boards
  (PAE addresses up to 64 GiB).
- Device memory (uncached MMIO): i686 PAE PTEs carry PWT|PCD exactly like x86_64;
  the Permissions.device field already flows; the encoder honors it (see §3). The
  unified carve-out + Device-map flow is arch-generic, so i686's device windows
  get carved from Normal and mapped uncached with no new logic.
- Device discovery / registry: i686 has no DTB. Its early device set is the
  STANDARD PC layout (VGA 0xB8000, COM1 0x3F8 is port-I/O not MMIO, PIC/APIC,
  PCI BARs via config space). Wire i686 to register its MMIO devices (local APIC
  0xFEE00000, IOAPIC 0xFEC00000, PCI BARs) into mmio_registry — the SAME registry
  the other arches use (it is already ungated for exactly this). This is where the
  registry's "x86 will populate it as discovery is wired" comment gets fulfilled.
- early_traps: build a 32-bit IDT (separate from x86_64's 64-bit IDT) so faults
  are reported, not silent. Arch-local, modest; do it before/with the MMU so MMU
  faults are diagnosable.
- Storage (mount_root_fs / verify_storage): i686 reuses the x86_64 port-I/O ATA
  driver almost verbatim (same in/out instructions, same PIO protocol) — likely
  the highest-reuse phase. Then the block layer is shared.
- ring-3 / isolation: i686 ring-3 transition (CPL0->CPL3 via iret with a TSS for
  ring0 stack) differs from x86_64's syscall/sysret but is well-trodden; mirror
  the x86_64 ring3_ctx structure with 32-bit segment selectors + TSS.

================================================================================
## 6. Insights captured (what was CORRECT vs CORRECTED across the arc)
CORRECTED (do not repeat):
  * FALSE "4 KiB mapping hangs" — MEASURED false (288K maps ~0.74s; boot ~1.07s;
    the 30s was QEMU not exiting). Large pages are a RESOURCE/scalability win, not
    a hang fix. For i686, justify PAE blocks by page-table frame savings on large
    PAE RAM, NOT by a hang.
  * "document the device edge case and move on" — REJECTED as a QEMU-era shortcut.
    For i686, a discovered/expected device the MMU mis-maps (Normal instead of
    Device) is a DEFECT to fix, not a footnote.
  * Treating i686 as a silent stub — CORRECTED; it is explicitly Skipped with
    honest reasons, and entropy/boot.s are at parity.
  * Fixed-size cap that silently drops device regions — CORRECTED to no-cap Vec +
    loud-fail registry. i686's device registration MUST use the same no-silent-drop
    discipline.
CORRECT (carry forward):
  * x86_64 is the REFERENCE implementation of the canon; build i686 ON x86_64's
    proven gates (it shares the most: PWT|PCD device bits, PS-bit blocks, NX,
    port-I/O ATA, PCI). Align by construction, not reconcile-after-drift.
  * Memory type is a FACT discovered from the platform and HONORED, not a policy
    default. i686: from BIOS/E820/PCI, mirroring x86_64.
  * Shared orchestration + per-arch DATA hooks. i686 adds DATA (geometry consts,
    device windows, CR3/CR4 install), not a parallel code path.
  * Verify each increment: build host+bare, tests pass, all-arch boots, no x86_64
    regression — BEFORE claiming done. Measure, don't assume.

================================================================================
## 7. Ordered execution plan (each step: build + 378 tests + 3-arch boots green
##  BEFORE moving on; keep x86_64/aarch64/riscv64 byte-identical)
1. i686 early_traps: 32-bit IDT. (Arch-local; low risk.)
2. Generalize portable paging over E::LEVELS / per-level index width (§2). KEEP
   the 3 working arches at (4,9) => byte-identical. Re-verify continuously.
3. Add paging_i686.rs PAE encoder WITH block + device support from the start (§3).
4. i686 ArchPaging impl + CR4.PAE/EFER.NXE/CR3 install sequence; route bring_up_mmu
   to bring_up_mmu_generic<PaePageTable> (§4). Boot i686 to "MMU online".
5. Wire i686 device registration (APIC/IOAPIC/PCI BARs) into mmio_registry (§5);
   confirm device windows carved + Device-mapped.
6. i686 ATA (reuse x86_64 port-I/O driver) -> mount_root_fs + verify_storage Done.
7. i686 ring-3 (TSS + iret to CPL3) -> isolation parity.
8. Real-hardware validation (32-bit PC / PAE-capable board) with serial-log
   capture per TESTING-GUIDE.md.
RESULT: true 4/4 — i686 a first-class arch using the identical shared
orchestration, with every arc capability (derived geometry, large pages, device
memory, unified carve/Device-map, entropy, bring-up contract) realized.
