# Large-page (2 MiB block) mapping — design

## Justification (MEASURED, honest — not the false "hang")
Identity-mapping RAM as 4 KiB pages needs page-table frames that scale with RAM:
  128 MiB -> 0.3 MiB tables (QEMU: trivial)
  8 GiB   -> 16 MiB tables
  32 GiB  -> 64 MiB tables
On a real board with multi-GiB RAM, that is a large chunk of RAM consumed by page
tables AND a large number of map iterations. 2 MiB block mappings place a leaf one
level up (skipping the L3 table entirely), cutting table overhead ~512x (32 GiB:
64 MiB -> ~128 KiB) and iterations 512x. THIS is the real bare-metal need: map big
real RAM cheaply. (NOT a hang fix — mapping speed was measured fine; this is
resource/scalability for real hardware.)

## Geometry recap
Portable layer: LEVELS=4, 9 bits/level, 4 KiB pages. Levels (0=top..3=leaf):
  L0 entry covers 512 GiB, L1 covers 1 GiB, L2 covers 2 MiB, L3 covers 4 KiB.
A 2 MiB block = a LEAF placed at L2 (index LEVELS-2), pointing at a 2 MiB-aligned
frame. A 1 GiB block = a leaf at L1 (LEVELS-3) — a later optional step.

## Per-arch block-descriptor formats (the only arch-specific part)
- x86_64: set PS (Page Size, bit 7) in the L2 entry; phys is 2 MiB-aligned. NX/RW/
  US as usual.
- aarch64 (VMSAv8-64): a BLOCK descriptor at L1/L2 uses bits[1:0]=0b01 (vs 0b11 for
  table/page). Same AF/SH/AP/attr/XN bits as a page leaf.
- riscv64 (Sv48): a leaf PTE (R/W/X != 0) placed at a non-final level IS a
  superpage; the PPN must be aligned to the level. Same flag bits as a 4 KiB leaf.

## Trait change (minimal, backward-compatible)
Add to PageTableEncoder:
    /// Encode a BLOCK/huge leaf at interior `level` (0=top). Default panics /
    /// unsupported so existing encoders are unaffected until they opt in.
    fn encode_block_leaf(frame: PhysFrame, perms: Permissions, level: usize) -> u64;
Provide it on all three working encoders. (No default that silently mis-encodes;
each arch implements it explicitly.)

## Walker change
Add AddressSpace::map_block_2m::<E>(virt, frame, perms, ...) that walks to L2
(LEVELS-2) and writes encode_block_leaf at level LEVELS-2. Guard: virt & frame
2 MiB-aligned; error if an entry already present at that slot (and never descend
into a slot already used as a table).
Then map_range uses 2 MiB blocks where (virt, phys, remaining) are all 2 MiB-
aligned, else 4 KiB. This keeps unaligned edges correct.

## Safety / verification plan (this is a CORE change)
1. Add encode_block_leaf to the trait + all 3 encoders. Build all arches.
2. Add map_block_2m + a unit test (host) that builds a small table set and checks
   the L2 entry is a block with correct frame+perms (and that a 4 KiB map into the
   same 2 MiB region after a block is rejected — no aliasing).
3. Switch the identity map in bring_up_mmu_generic to 2 MiB blocks for the aligned
   bulk, 4 KiB for any remainder.
4. RE-VERIFY all three arches boot byte-identical (x86 full stack; aarch64/riscv64
   MMU online) and the map size shrinks the page-table frame count. Only then keep.
Roll back instantly if any arch regresses — the 4 KiB path stays as the fallback.

---

## RESULT — verified across the 3 implemented arches
encode_block_leaf + is_block_leaf added to PageTableEncoder and all 5 impls
(x86_64 PS bit; aarch64 block descriptor bits[1:0]=01; riscv64 superpage PTE; 2
test encoders). map_block_2m walker + map_range using 2 MiB blocks for the aligned
bulk, 4 KiB for edges. translate made block-aware (stops at a block leaf at an
interior level instead of descending into it as a table).

VERIFIED:
  - 377 tests pass (+2 new): one proves a 2 MiB-aligned range becomes a single
    block leaf at L2 and translates correctly across the whole 2 MiB (incl an
    offset); one proves an unaligned range falls back to 4 KiB (L2 stays a table).
  - x86_64 FULL stack with blocks active: MMU online, CONTAINER ISOLATION VERIFIED
    (exercises the block-aware translate), DNS STACK OK, REMOTE LINK OK, boot
    complete — byte-identical behavior.
  - aarch64 + riscv64: MMU online + boot complete with blocks active.
All three build clean (0 warnings).

## How this works for i686 (the 4th arch) — design now, so the path is coherent
i686 has NO PageTableEncoder impl yet (its MMU phase returns Skipped), so the
trait additions do not affect it today. But when i686's MMU is built, large pages
must be considered from the start, NOT bolted on:

KEY i686 FACTS that differ from the 4-level arches:
  - 32-bit paging geometry does NOT match the portable LEVELS=4/INDEX_BITS=9/8-byte
    model. Two real options:
      (a) CLASSIC 32-bit: 2-level (10+10+12), 4-BYTE entries. Large page = a 4 MiB
          page via the PSE PS bit in the page directory entry (one level up). So
          i686 classic's "block" is 4 MiB, not 2 MiB, and entries are u32.
      (b) PAE: 3-level (2+9+9+12), 8-BYTE entries. Large page = a 2 MiB page via
          the PS bit in the PD entry. PAE's 2 MiB block matches x86_64's 2 MiB
          size and 8-byte entry width — the CLOSEST fit to the portable model.
  - Either way the PORTABLE map_range/map_block_2m assume LEVELS=4 and u64 entries.
    i686 cannot reuse them as-is.

IMPLICATION for the design (so we don't drift): when i686's MMU lands, the clean
path is to generalize the portable paging layer over (LEVELS, entry-width) — i.e.
make LEVELS and the entry integer type per-arch associated constants/types on the
encoder, with map_block_2m generalized to "place a block leaf at the arch's
block level" (L2 for 4-level 2 MiB; PD for PAE 2 MiB; PD for classic 4 MiB). The
encode_block_leaf signature already takes `level`, which is forward-compatible
with this. PAE (option b) is recommended because its 2 MiB block + 8-byte entries
align with the existing block machinery — minimal new surface.

So large pages are NOT an x86_64/aarch64/riscv64-only feature bolted on ahead of
i686: the trait shape (encode_block_leaf(frame, perms, LEVEL)) was chosen to
extend to i686's eventual encoder. The remaining i686 work (per
I686-PARITY-ACCOUNTING.md) is: generalize LEVELS+entry-width, add the i686 (PAE)
encoder INCLUDING encode_block_leaf/is_block_leaf from the start, then wire
bring_up_mmu. No i686 shortcut; the block design already accounts for it.

---

## Hazard found + fixed during dwelling (no-shortcut discipline)
While reviewing for correctness, found a latent corruption hazard: AddressSpace::
map() walked interior levels and, on a present entry, descended via entry_frame()
treating it as a table. If a 2 MiB BLOCK already occupied that interior slot, a
later 4 KiB map() into the same region would descend INTO THE BLOCK'S MAPPED FRAME
as if it were a page table — corrupting mapped memory. The current map_range never
triggers this (blocks and 4 KiB regions don't share an L2 slot), but map() is
public and a future caller (user-space mapping, i686, etc.) could.
FIX: map()'s interior walk now checks is_block_leaf(entry, level) and returns an
error ("address covered by an existing large-page block") instead of descending.
TEST: map_4k_into_existing_block_is_rejected proves a 4 KiB map into a region
already covered by a 2 MiB block errors (no corruption). 378 tests pass.
This is exactly the kind of bare-metal correctness issue that the "no QEMU-era
shortcut, dwell on correctness" discipline is meant to catch — it would never
manifest in the current QEMU boot but would be a real defect on a system that
mixes block and page mappings.
