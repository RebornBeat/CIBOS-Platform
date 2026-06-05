/* CIBOS Stage 2 bootloader.
 *
 * Entry: 16-bit real mode at physical 0x8000, with DL = boot drive and SI =
 * pointer to the Boot Layout Descriptor (0x0600). All BIOS-service work (A20,
 * the E820 memory map, disk reads) happens here in real mode, because the BIOS
 * services vanish once we leave it. After loading the images and building the
 * handoff we make a single one-way transition out of real mode and jump to the
 * CIBIOS entry recorded in the descriptor.
 *
 * Build two variants from this one source (see build.sh):
 *   x86_64 (default)        -> ends in 64-bit long mode, identity-mapped 0..4 GiB,
 *                              the BootHandoff pointer in RDI.
 *   i686  (-DCIBOS_BOOT32)  -> ends in 32-bit protected mode, flat segments,
 *                              paging off, the BootHandoff pointer in EAX.
 *
 * Physical memory map used during Stage 2:
 *   0x00600..0x00667  Boot Layout Descriptor (placed by Stage 1; 104 bytes)
 *   0x02000..0x02057  BootHandoff (88 bytes)
 *   0x07000           real-mode stack top (grows down)
 *   0x08000..         this code/data (kept <= 32 KiB; build.sh asserts)
 *   0x10000..0x11FFF  E820 region array (BootMemoryRegion[24] each; cap 256)
 *   0x20000..0x2FFFF  64 KiB disk read scratch
 *   0x30000..0x35FFF  page tables (PML4 + PDPT + 4 PDs), x86_64 only
 *   0x70000           long/protected-mode stack top
 *
 * Assembled with `gcc -m32` (C preprocessor + GNU as, Intel syntax).
 */

    .intel_syntax noprefix

/* Boot Layout Descriptor field offsets — mirror
 * shared::protocols::boot::BootLayoutDescriptor. */
#define BLD              0x0600
#define BLD_STAGE2_SECT  (BLD + 24)
#define BLD_STAGE2_ADDR  (BLD + 32)
#define BLD_CIBIOS_LBA   (BLD + 40)
#define BLD_CIBIOS_SECT  (BLD + 48)
#define BLD_CIBIOS_ADDR  (BLD + 56)
#define BLD_CIBIOS_ENTRY (BLD + 64)
#define BLD_CIBOS_LBA    (BLD + 72)
#define BLD_CIBOS_SECT   (BLD + 80)
#define BLD_CIBOS_ADDR   (BLD + 88)
#define BLD_CIBOS_SIZE   (BLD + 96)

/* BootHandoff at 0x2000 — offsets mirror
 * shared::protocols::boot::BootHandoff. */
#define HO               0x2000
#define HO_MAGIC         (HO + 0)
#define HO_VERSION       (HO + 8)
#define HO_FLAGS         (HO + 12)
#define HO_DRIVE         (HO + 16)
#define HO_MEMCOUNT      (HO + 20)
#define HO_MEMPTR        (HO + 24)
#define HO_CIBOS_ADDR    (HO + 32)
#define HO_CIBOS_SIZE    (HO + 40)
#define HO_CIBIOS_ADDR   (HO + 48)
#define HO_CIBIOS_SIZE   (HO + 56)
#define HO_PTROOT        (HO + 64)
#define HO_S2ADDR        (HO + 72)
#define HO_S2SIZE        (HO + 80)

#define E820_SEG         0x1000      /* segment base -> physical 0x10000 */
#define E820_PHYS        0x10000
#define DISK_SEG         0x2000      /* segment base -> physical 0x20000 */
#define DISK_PHYS        0x20000
#define PT_BASE          0x30000
#define LM_STACK         0x70000
#define CHUNK_SECTORS    64          /* 32 KiB per disk read; no 64 KiB crossing */
#define MAX_E820         256

    .code16
    .section .text16, "ax"
    .global stage2_start

stage2_start:
    cli
    xor     ax, ax
    mov     ds, ax
    mov     es, ax
    mov     ss, ax
    mov     sp, 0x7000
    cld
    mov     [bsp_drive], dl
    sti

    lea     si, [msg_s2]
    call    s2print

    call    a20_enable
    call    e820_collect
    call    images_load
    call    handoff_build
    call    paging_setup
    jmp     mode_switch          /* never returns */

/* ---- A20: try the BIOS service, then the fast-A20 gate via port 0x92 ---- */
a20_enable:
    mov     ax, 0x2401
    int     0x15
    in      al, 0x92
    test    al, 2
    jnz     a20_done
    or      al, 2
    and     al, 0xFE             /* never set bit 0 (would trigger a fast reset) */
    out     0x92, al
a20_done:
    ret

/* ---- Establish (or re-establish) unreal mode: load a 4 GiB limit into the
 *      FS/GS descriptor caches via a brief protected-mode excursion, then
 *      return to real mode without reloading FS/GS. Called fresh before each
 *      high-memory copy so an intervening BIOS call cannot invalidate the
 *      cached descriptor. ---- */
enter_unreal:
    push    eax
    push    bx
    cli
    lgdt    [gdt32_ptr]
    mov     eax, cr0
    or      al, 1
    mov     cr0, eax
    mov     bx, 0x10             /* flat 4 GiB data selector */
    mov     fs, bx
    mov     gs, bx
    mov     eax, cr0
    and     al, 0xFE
    mov     cr0, eax
    sti
    pop     bx
    pop     eax
    ret

/* ---- Collect the BIOS E820 memory map into BootMemoryRegion[] at 0x10000.
 *      Each BIOS entry is requested as 24 bytes, which is byte-identical to a
 *      BootMemoryRegion, so entries are stored verbatim. ---- */
e820_collect:
    push    es
    mov     ax, E820_SEG
    mov     es, ax
    xor     di, di
    xor     ebx, ebx
    xor     bp, bp
    mov     edx, 0x534D4150      /* 'SMAP' */
    mov     eax, 0xE820
    mov     dword ptr es:[di + 20], 1   /* ask for the ACPI attribute dword */
    mov     ecx, 24
    int     0x15
    jc      e820_fail
    cmp     eax, 0x534D4150
    jne     e820_fail
    jmp     e820_check
e820_loop:
    mov     edx, 0x534D4150
    mov     eax, 0xE820
    mov     dword ptr es:[di + 20], 1
    mov     ecx, 24
    int     0x15
    jc      e820_done
e820_check:
    jcxz    e820_skip            /* zero-length descriptor: ignore */
    cmp     ecx, 20
    jbe     e820_store           /* 20-byte (legacy) entry: always valid */
    test    byte ptr es:[di + 20], 1
    jz      e820_skip            /* ACPI bit 0 clear: ignore this entry */
e820_store:
    inc     bp
    cmp     bp, MAX_E820
    jae     e820_done
    add     di, 24
e820_skip:
    test    ebx, ebx
    jnz     e820_loop
e820_done:
    mov     [e820_count], bp
    pop     es
    ret
e820_fail:
    mov     word ptr [e820_count], 0
    pop     es
    ret

/* ---- Load CIBIOS, then CIBOS, to their high-memory destinations ---- */
images_load:
    mov     eax, [BLD_CIBIOS_LBA]
    mov     edx, [BLD_CIBIOS_LBA + 4]
    mov     ecx, [BLD_CIBIOS_SECT]
    mov     esi, [BLD_CIBIOS_ADDR]
    call    load_one_image
    mov     eax, [BLD_CIBOS_LBA]
    mov     edx, [BLD_CIBOS_LBA + 4]
    mov     ecx, [BLD_CIBOS_SECT]
    mov     esi, [BLD_CIBOS_ADDR]
    call    load_one_image
    ret

/* load_one_image(eax:edx = start LBA, ecx = sectors, esi = dest physical).
 * Loop state lives in memory so it survives INT 13h and enter_unreal. */
load_one_image:
    mov     [cur_lba_lo], eax
    mov     [cur_lba_hi], edx
    mov     [cur_rem], ecx
    mov     [cur_dest], esi
li_loop:
    mov     ecx, [cur_rem]
    test    ecx, ecx
    jz      li_done
    mov     ebx, ecx
    cmp     ebx, CHUNK_SECTORS
    jbe     li_have
    mov     ebx, CHUNK_SECTORS
li_have:
    mov     [cur_chunk], ebx
    mov     ax, bx
    mov     [dap_count], ax
    mov     word ptr [dap_off], 0
    mov     word ptr [dap_seg], DISK_SEG
    mov     eax, [cur_lba_lo]
    mov     [dap_lba_lo], eax
    mov     eax, [cur_lba_hi]
    mov     [dap_lba_hi], eax
    lea     si, [dap]
    mov     ah, 0x42
    mov     dl, [bsp_drive]
    int     0x13
    jc      disk_err
    /* copy this chunk from the low scratch buffer up to its destination */
    call    enter_unreal
    mov     esi, DISK_PHYS
    mov     edi, [cur_dest]
    mov     ecx, [cur_chunk]
    shl     ecx, 9               /* sectors -> bytes */
    add     ecx, 3
    shr     ecx, 2               /* bytes   -> dwords (round up) */
    call    unreal_copy
    /* advance LBA, destination, and remaining count */
    mov     eax, [cur_chunk]
    add     [cur_lba_lo], eax
    adc     dword ptr [cur_lba_hi], 0
    mov     eax, [cur_chunk]
    shl     eax, 9
    add     [cur_dest], eax
    mov     eax, [cur_chunk]
    sub     [cur_rem], eax
    jmp     li_loop
li_done:
    ret

/* unreal_copy(esi = src phys, edi = dst phys, ecx = dword count) via flat FS. */
unreal_copy:
uc_loop:
    test    ecx, ecx
    jz      uc_done
    mov     eax, fs:[esi]
    mov     fs:[edi], eax
    add     esi, 4
    add     edi, 4
    dec     ecx
    jmp     uc_loop
uc_done:
    ret

/* ---- Build the BootHandoff at 0x2000. All targets are < 64 KiB, so the
 *      real-mode DS (0) addresses them directly. ---- */
handoff_build:
    mov     dword ptr [HO_MAGIC], 0x4F424943      /* 'C','I','B','O' */
    mov     dword ptr [HO_MAGIC + 4], 0x314F4853  /* 'S','H','O','1' */
    mov     dword ptr [HO_VERSION], 1
    mov     dword ptr [HO_FLAGS], 0
    movzx   eax, byte ptr [bsp_drive]
    mov     [HO_DRIVE], eax
    movzx   eax, word ptr [e820_count]
    mov     [HO_MEMCOUNT], eax
    mov     dword ptr [HO_MEMPTR], E820_PHYS
    mov     dword ptr [HO_MEMPTR + 4], 0
    /* CIBOS image addr/size (exact byte size copied verbatim from descriptor) */
    mov     eax, [BLD_CIBOS_ADDR]
    mov     [HO_CIBOS_ADDR], eax
    mov     eax, [BLD_CIBOS_ADDR + 4]
    mov     [HO_CIBOS_ADDR + 4], eax
    mov     eax, [BLD_CIBOS_SIZE]
    mov     [HO_CIBOS_SIZE], eax
    mov     eax, [BLD_CIBOS_SIZE + 4]
    mov     [HO_CIBOS_SIZE + 4], eax
    /* CIBIOS image addr/size (size = sectors * 512) */
    mov     eax, [BLD_CIBIOS_ADDR]
    mov     [HO_CIBIOS_ADDR], eax
    mov     eax, [BLD_CIBIOS_ADDR + 4]
    mov     [HO_CIBIOS_ADDR + 4], eax
    mov     eax, [BLD_CIBIOS_SECT]
    shl     eax, 9
    mov     [HO_CIBIOS_SIZE], eax
    mov     dword ptr [HO_CIBIOS_SIZE + 4], 0
    /* page-table root (zero on i686 where paging stays off) */
#ifdef CIBOS_BOOT32
    mov     dword ptr [HO_PTROOT], 0
#else
    mov     dword ptr [HO_PTROOT], PT_BASE
#endif
    mov     dword ptr [HO_PTROOT + 4], 0
    /* Stage 2 addr/size */
    mov     eax, [BLD_STAGE2_ADDR]
    mov     [HO_S2ADDR], eax
    mov     dword ptr [HO_S2ADDR + 4], 0
    mov     eax, [BLD_STAGE2_SECT]
    shl     eax, 9
    mov     [HO_S2SIZE], eax
    mov     dword ptr [HO_S2SIZE + 4], 0
    ret

#ifndef CIBOS_BOOT32
/* ---- x86_64: identity-map 0..4 GiB with 2 MiB pages. ----
 * PML4 @ 0x30000, PDPT @ 0x31000, PD0..PD3 @ 0x32000..0x35000. */
paging_setup:
    call    enter_unreal
    /* zero 0x30000..0x36000 (6 pages = 0x6000 bytes) */
    mov     edi, PT_BASE
    mov     ecx, 0x6000 / 4
    xor     eax, eax
pz_loop:
    mov     fs:[edi], eax
    add     edi, 4
    dec     ecx
    jnz     pz_loop
    /* PML4[0] -> PDPT | present | rw */
    mov     edi, PT_BASE
    mov     dword ptr fs:[edi], (PT_BASE + 0x1000 + 0x3)
    mov     dword ptr fs:[edi + 4], 0
    /* PDPT[0..3] -> PD0..PD3 | present | rw */
    mov     edi, (PT_BASE + 0x1000)
    mov     eax, (PT_BASE + 0x2000)
    mov     ecx, 4
pdpt_loop:
    mov     ebx, eax
    or      ebx, 0x3
    mov     fs:[edi], ebx
    mov     dword ptr fs:[edi + 4], 0
    add     edi, 8
    add     eax, 0x1000
    dec     ecx
    jnz     pdpt_loop
    /* PD entries: 4 * 512 = 2048 entries, 2 MiB pages, present|rw|ps (0x83) */
    mov     edi, (PT_BASE + 0x2000)
    xor     eax, eax             /* current 2 MiB page base (low dword) */
    mov     ecx, 2048
pd_loop:
    mov     ebx, eax
    or      ebx, 0x83
    mov     fs:[edi], ebx
    mov     dword ptr fs:[edi + 4], 0   /* all bases < 4 GiB, high dword 0 */
    add     edi, 8
    add     eax, 0x200000
    dec     ecx
    jnz     pd_loop
    ret

/* ---- x86_64: enter long mode and jump to CIBIOS with RDI = &BootHandoff. ---- */
mode_switch:
    cli
    lgdt    [gdt64_ptr]
    mov     eax, PT_BASE
    mov     cr3, eax
    mov     eax, cr4
    or      eax, 0x20            /* CR4.PAE */
    mov     cr4, eax
    mov     ecx, 0xC0000080      /* EFER */
    rdmsr
    or      eax, 0x100           /* EFER.LME */
    wrmsr
    mov     eax, cr0
    or      eax, 0x80000001      /* CR0.PG | CR0.PE */
    mov     cr0, eax
    /* far jump to the 64-bit code selector (0x08) with a 32-bit offset, encoded
     * by hand as ptr16:32 (0x66 0xEA imm32 sel16) because the assembler does not
     * accept the `selector:label` mnemonic form here. */
    .byte   0x66, 0xEA
    .long   long_entry
    .word   0x08

    .code64
long_entry:
    mov     ax, 0x10
    mov     ds, ax
    mov     es, ax
    mov     ss, ax
    mov     fs, ax
    mov     gs, ax
    mov     rsp, LM_STACK
    mov     rdi, HO              /* arg0 = &BootHandoff (System V) */
    mov     rax, [BLD_CIBIOS_ENTRY]
    jmp     rax
#else
/* ---- i686: no paging; enter flat 32-bit protected mode, EAX = &BootHandoff. ---- */
paging_setup:
    ret

mode_switch:
    cli
    lgdt    [gdt32_ptr]
    mov     eax, cr0
    or      al, 1                /* CR0.PE */
    mov     cr0, eax
    .byte   0x66, 0xEA
    .long   pm_entry
    .word   0x08

    .code32
pm_entry:
    mov     ax, 0x10
    mov     ds, ax
    mov     es, ax
    mov     ss, ax
    mov     fs, ax
    mov     gs, ax
    mov     esp, LM_STACK
    mov     ebx, [BLD_CIBIOS_ENTRY]   /* low 32 bits of the CIBIOS entry */
    mov     eax, HO                   /* CIBIOS x86 entry reads handoff from EAX */
    jmp     ebx
#endif

/* ---- shared 16-bit helpers and data ---- */
    .code16
s2print:
    push    ax
    push    bx
    push    si
    mov     ah, 0x0E
    mov     bx, 0x0007
sp_loop:
    lodsb
    test    al, al
    jz      sp_done
    int     0x10
    jmp     sp_loop
sp_done:
    pop     si
    pop     bx
    pop     ax
    ret

disk_err:
    lea     si, [msg_derr]
    call    s2print
de_loop:
    hlt
    jmp     de_loop

    .align 16
/* 32-bit flat GDT: used for unreal mode, and as the final GDT on i686. */
gdt32:
    .quad   0x0000000000000000
    .quad   0x00CF9A000000FFFF   /* 0x08: 32-bit code, base 0, limit 4 GiB */
    .quad   0x00CF92000000FFFF   /* 0x10: 32-bit data, base 0, limit 4 GiB */
gdt32_end:
gdt32_ptr:
    .word   gdt32_end - gdt32 - 1
    .long   gdt32

    .align 16
/* 64-bit GDT: used for the long-mode transition on x86_64. */
gdt64:
    .quad   0x0000000000000000
    .quad   0x00AF9A000000FFFF   /* 0x08: 64-bit code (L=1) */
    .quad   0x00CF92000000FFFF   /* 0x10: data */
gdt64_end:
gdt64_ptr:
    .word   gdt64_end - gdt64 - 1
    .long   gdt64

    .align 2
bsp_drive:
    .byte   0
    .align 2
e820_count:
    .word   0
    .align 4
cur_lba_lo:
    .long   0
cur_lba_hi:
    .long   0
cur_rem:
    .long   0
cur_dest:
    .long   0
cur_chunk:
    .long   0
    .align 2
dap:
    .byte   0x10
    .byte   0
dap_count:
    .word   0
dap_off:
    .word   0
dap_seg:
    .word   0
dap_lba_lo:
    .long   0
dap_lba_hi:
    .long   0
msg_s2:
    .asciz  "CIBOS bootloader stage 2\r\n"
msg_derr:
    .asciz  "stage2: disk read error\r\n"
