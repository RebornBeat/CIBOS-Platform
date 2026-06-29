// CIBOS kernel AArch64 boot entry (EL1).
//
// This image begins with the standard ARM64 Linux image header (see the arm64
// boot protocol). A conforming boot loader — real U-Boot/UEFI on hardware, and
// QEMU `virt` — parses this header, loads the image at a 2 MiB-aligned base, and
// jumps to _start with:
//     x0 = physical address of the DTB (device tree blob)
//     x1 = x2 = x3 = 0
// So the DTB pointer arrives in x0 on EVERY conforming platform (hardware AND
// QEMU), which is why the kernel needs NO hardcoded "QEMU fallback" for the DTB.
//
// The CIBIOS firmware path uses a separate entry/handoff and does not rely on
// this header (it calls the kernel with its own handoff pointer).
.section .text.boot
.global _start
_start:
    // --- ARM64 image header (64 bytes) ---
    b    real_start          // code0: branch to the real entry (also valid code)
    .long 0                  // code1 (reserved when code0 is a branch)
    .quad 0x80000            // text_offset: 512 KiB (matches 0x40080000 vs RAM base 0x40000000)
    .quad _image_size        // image_size (effective size of the loaded image)
    .quad 0x2                // flags: bit0=0 LE, bits[1:2]=01 4 KiB pages, bit3=0
                             // (text_offset HONORED — the kernel is linked at a
                             // fixed address 0x40080000 and is position-dependent,
                             // so the loader must place it at RAM_base+text_offset,
                             // NOT "anywhere"). Setting bit3 would let a conforming
                             // loader relocate us and break absolute symbol refs.
    .quad 0                  // res2
    .quad 0                  // res3
    .quad 0                  // res4
    .long 0x644d5241         // magic: "ARM\x64"
    .long 0                  // res5 (PE header offset; unused)

real_start:
    // The DTB pointer is in x0 (per the arm64 boot protocol). Preserve it.
    mov  x19, x0

    // Set up the stack.
    adrp x1, __stack_top
    add  x1, x1, :lo12:__stack_top
    mov  sp, x1

    // Clear BSS.
    adrp x1, __bss_start
    add  x1, x1, :lo12:__bss_start
    adrp x2, __bss_end
    add  x2, x2, :lo12:__bss_end
1:  cmp  x1, x2
    b.ge 2f
    str  xzr, [x1], #8
    b    1b
2:
    // Enable FP/SIMD access at EL1 (CPACR_EL1.FPEN = 0b11) before any Rust code,
    // which uses SIMD registers in core (fmt/memcpy).
    mrs  x1, cpacr_el1
    orr  x1, x1, #(0b11 << 20)
    msr  cpacr_el1, x1
    isb

    // kernel_entry(handoff_ptr, dtb_ptr). On this self-boot path there is no
    // firmware handoff, so handoff_ptr = 0 and the kernel synthesizes the handoff
    // from the DTB (which is the real platform description). dtb_ptr = x19 (the
    // DTB the boot loader passed in x0).
    mov  x0, xzr            // handoff_ptr = 0 (self-boot: synthesize from DTB)
    mov  x1, x19            // dtb_ptr = the DTB pointer
    bl   kernel_entry
3:  wfe
    b    3b
