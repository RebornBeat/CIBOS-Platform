# i686 (32-bit x86) — honest parity accounting

You're right that i686 has been 3/4, not 4/4. Here is the honest state and WHY,
with no stub-hand-waving.

## Where i686 stands today
- Serial console (COM1 0x3F8 via port I/O) + halt: DONE, identical pattern to the
  other arches.
- ArchBringUp impl present, but phases returned Skipped.
- seed_entropy: NOW WIRED to the shared portable helper (this session) — i686 is
  at parity on entropy, identical code to every arch (the CSPRNG is pure
  cibos_kernel, zero arch specifics). 1/5 -> done.
- early_traps (32-bit IDT), bring_up_mmu (32-bit paging), mount_root_fs/
  verify_storage (block driver): still Skipped.

## The REAL reason i686 can't just reuse the shared MMU orchestration
The portable paging layer (cibos_kernel::paging) assumes a fixed geometry:
  LEVELS=4, INDEX_BITS=9 (9 bits/level), 8-byte (u64) entries, 4 KiB pages.
This matches x86_64 (4-level), aarch64 (4 KiB granule, 4-level), riscv64 (Sv48,
4-level) — they share ONE page-walk by design.

32-bit i686 paging does NOT fit this:
  - Classic 32-bit: 2-level (10+10+12 bits), 4-BYTE entries. Different level count
    AND different entry width — the portable PageTableEncoder returns u64 and the
    walker does 4 levels. Incompatible.
  - PAE 32-bit: 3-level (2+9+9+12), 8-byte entries. 8-byte matches, but 3 levels
    (not 4) and a 2-bit top index — still doesn't fit LEVELS=4/INDEX_BITS=9.

So i686 MMU bring-up is a GENUINE architectural divergence, not laziness. It needs
either:
  (A) a 32-bit-specific paging path (its own 2-level or PAE encoder + a walker that
      honors a per-arch LEVELS/entry-width), which means generalizing the portable
      layer over LEVELS and entry type — a real change to the core; or
  (B) accept i686 maps via the firmware/identity path without the kernel rebuilding
      page tables (the firmware's paging stays active), i.e. i686 runs with MMU
      managed at the CIBIOS layer, not the CIBOS kernel layer.

## Honest recommendation
Option (A) is the "true 4/4" but it touches the core paging contract (make it
generic over LEVELS and entry width: u32 vs u64). That is doable and aligned with
the no-drift philosophy (one generic walker, per-arch geometry constants), but it
is a CORE change that must be done carefully and fully tested on the existing 3
arches to prove no regression — exactly the kind of thing to do deliberately, not
rushed at the end of a session.

The cheap wins (entropy now; a 32-bit IDT for early_traps next) bring i686 closer
without touching the core. The MMU parity is the one real piece of work, scoped
honestly as "generalize the portable paging geometry over LEVELS + entry width."

## So the plan to reach true 4/4
1. (done) entropy via shared helper.
2. i686 early_traps: a 32-bit IDT (separate from x86_64's 64-bit IDT) so faults
   are reported — modest, arch-local.
3. Generalize cibos_kernel::paging over LEVELS + entry-width (u32/u64); add an
   i686 encoder (start with PAE 3-level/8-byte as the closest fit, or classic
   2-level). Re-verify x86_64/aarch64/riscv64 byte-identical FIRST.
4. Wire i686 bring_up_mmu to the generalized orchestration.
5. Block driver for mount_root_fs/verify_storage (shared ATA logic; i686 uses the
   same port-I/O ATA as x86_64 — likely highly reusable).

---

## This session's i686 progress (verified)
1. seed_entropy: WIRED to the shared portable helper (was Skipped). i686 now seeds
   the CSPRNG with the identical code as every other arch. VERIFIED: i686 firmware
   + i686 kernel image both build.
2. FOUND + FIXED a real bug: boot/x86.s still used the OLD single-arg
   kernel_entry(handoff_ptr) signature, but kernel_entry became 2-arg
   (handoff_ptr, dtb_ptr) last session. On 32-bit cdecl that meant the kernel read
   a garbage second argument off the stack. Fixed boot/x86.s to push both u64 args
   (dtb=0, since x86 has no device tree). VERIFIED: i686 kernel image now links and
   builds clean. This was latent because i686's kernel boot isn't in the QEMU
   sweep — exactly the kind of drift to catch.

## Still remaining for true i686 4/4 (honestly scoped, not rushed)
- early_traps: 32-bit IDT (arch-local, modest).
- bring_up_mmu: requires generalizing cibos_kernel::paging over LEVELS + entry
  width (i686 is 2-level/4-byte classic or 3-level/8-byte PAE — neither fits the
  4-level/8-byte portable geometry). This is a CORE change; do it deliberately,
  re-verifying the other 3 arches byte-identical first.
- mount_root_fs/verify_storage: i686 can reuse the x86_64 port-I/O ATA driver
  (same in/out instructions) — likely high reuse.

i686 is no longer silently "a stub": it has serial + entropy at parity and a
correct 2-arg entry, with the MMU divergence documented as the one real piece of
core work remaining.
