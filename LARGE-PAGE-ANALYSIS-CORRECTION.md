# Correction: the "4 KiB pages hang" claim was WRONG — measured the truth

## What I claimed last session (incorrectly)
"We already hit this wall on aarch64 — mapping 4 GiB as 4 KiB pages was so slow
it looked like a hang. A real board with 8 GiB would try ~2M mappings and hang /
exhaust frames."

## What actually happened (the real record)
The early aarch64 "silent stop after frame allocator" was NOT caused by 4 KiB
mapping slowness. The documented root causes were:
  1. Wrong RAM base (synth handoff said 1 MiB; aarch64 RAM is at 1 GiB) -> frame
     allocator handed out non-existent frames.
  2. Wrong reservation watermark (reserved below 64 MiB protected nothing when the
     kernel is at 1 GiB) -> table build clobbered the kernel.
  3. Double-mapped MMIO ("already mapped").
The "4 GiB attempt didn't finish" was attributed to slowness but NEVER MEASURED.
The real bugs (1,2,3) were present at the same time and are what actually broke it.

## The measurement (done now, not asserted)
Current aarch64 maps ~1.1 GiB = ~288K 4 KiB pages.
  - Time to REACH "boot complete": ~1.07 seconds total.
  - Time from "frame allocator" to "page tables built" (the actual mapping work):
    ~0.74 seconds for 288K pages.
  - The "30s" seen earlier was QEMU NOT EXITING after the kernel halts (timeout
    fires), NOT the boot being slow.

## Corrected conclusion
- Mapping ~288K pages takes < 1 second. Linear extrapolation: 8 GiB (~2M pages)
  ~= 5 seconds in TCG EMULATION; far faster on real silicon (no emulation tax).
- That is SLOW for huge RAM but NOT a hang and NOT a correctness failure.
- Therefore large-page (2 MiB / 1 GiB block) mapping is a legitimate PERFORMANCE
  optimization that real kernels use, NOT a fix for a non-existent hang. It should
  be done for production polish and to keep boot fast on big-RAM boards, but it is
  not blocking and must not be justified by a false "it hangs" premise.

## Lesson (alignment)
Do not build a fix on an unmeasured failure narrative. Measure first. The value of
dwelling is catching exactly this: a plausible-sounding hazard that, when actually
tested, is milder than claimed. The bare-metal goal is still served by large pages
(faster boot, fewer page-table frames on big-RAM boards) — but framed honestly as
optimization + scalability, not "unhang the kernel."
