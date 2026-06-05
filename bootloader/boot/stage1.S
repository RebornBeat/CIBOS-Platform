/* CIBOS Stage 1 bootloader (MBR boot sector, 512 bytes).
 *
 * BIOS loads this at physical 0x7C00 in 16-bit real mode with the boot drive
 * number in DL. Responsibilities, in order:
 *   1. Establish a known segment/stack state and preserve the boot drive.
 *   2. Confirm INT 13h EDD (LBA) extensions are present (USB boot implies them).
 *   3. Read the Boot Layout Descriptor from LBA 1 to physical 0x0600.
 *   4. Validate the descriptor magic ("CIBOSBL1").
 *   5. Load Stage 2 (LBA / sector count / address from the descriptor) one
 *      sector at a time, to stay well clear of BIOS 64 KiB DMA-boundary quirks.
 *   6. Jump to Stage 2 with DL = boot drive, SI = descriptor pointer (0x0600).
 *
 * Must fit in 446 bytes of code/data (the partition table occupies bytes
 * 446..510 and the 0xAA55 signature bytes 510..512); the link script places the
 * signature, and build.sh asserts the final size is exactly 512.
 *
 * Assembled with `gcc -m32` (C preprocessor + GNU as, Intel syntax) and linked
 * flat by `link/stage1.ld`. See build.sh.
 */

    .intel_syntax noprefix
    .code16
    .section .text._start, "ax"
    .global _start

/* Boot Layout Descriptor lands at 0x0600. Offsets mirror
 * shared::protocols::boot::BootLayoutDescriptor exactly (which is
 * #[repr(C, align(8))], so these byte offsets are identical on i686/x86_64). */
#define BLD             0x0600
#define BLD_MAGIC_LO    (BLD + 0)        /* "CIBO" */
#define BLD_MAGIC_HI    (BLD + 4)        /* "SBL1" */
#define BLD_STAGE2_LBA  (BLD + 16)
#define BLD_STAGE2_SECT (BLD + 24)
#define BLD_STAGE2_ADDR (BLD + 32)

_start:
    cli
    xor     ax, ax
    mov     ds, ax
    mov     es, ax
    mov     ss, ax
    mov     sp, 0x7C00          /* stack grows down from just below our load */
    cld
    mov     [boot_drive], dl
    sti

    /* --- Confirm INT 13h EDD / LBA extensions are present --- */
    mov     ah, 0x41
    mov     bx, 0x55AA
    mov     dl, [boot_drive]
    int     0x13
    jc      err_edd
    cmp     bx, 0xAA55
    jne     err_edd

    /* --- Read the descriptor: LBA 1, 1 sector, to 0x0600 --- */
    mov     word ptr [dap_count], 1
    mov     word ptr [dap_off], BLD
    mov     word ptr [dap_seg], 0
    mov     dword ptr [dap_lba_lo], 1
    mov     dword ptr [dap_lba_hi], 0
    call    read_dap

    /* --- Validate magic "CIBOSBL1" (little-endian dwords) --- */
    mov     eax, [BLD_MAGIC_LO]
    cmp     eax, 0x4F424943         /* 'C','I','B','O' */
    jne     err_bld
    mov     eax, [BLD_MAGIC_HI]
    cmp     eax, 0x314C4253         /* 'S','B','L','1' */
    jne     err_bld

    /* --- Load Stage 2, one sector per iteration --- */
    mov     ecx, [BLD_STAGE2_SECT]   /* sector count (low 16 bits used) */
    mov     eax, [BLD_STAGE2_LBA]     /* LBA low  */
    mov     edx, [BLD_STAGE2_LBA + 4] /* LBA high */
    mov     di, [BLD_STAGE2_ADDR]     /* dest offset, segment 0 (addr <= 0xFFFF) */
load_loop:
    test    cx, cx
    jz      load_done
    mov     word ptr [dap_count], 1
    mov     [dap_off], di
    mov     word ptr [dap_seg], 0
    mov     [dap_lba_lo], eax
    mov     [dap_lba_hi], edx
    call    read_dap
    add     di, 512
    add     eax, 1
    adc     edx, 0
    dec     cx
    jmp     load_loop
load_done:
    mov     dl, [boot_drive]
    mov     si, BLD
    jmp     word ptr [BLD_STAGE2_ADDR]   /* near indirect jump, CS = 0 */

/* Read one sector described by the DAP. Preserves the 32-bit loop registers
 * across the BIOS call (some BIOSes clobber high halves). */
read_dap:
    push    eax
    push    ecx
    push    edx
    push    di
    lea     si, [dap]
    mov     ah, 0x42
    mov     dl, [boot_drive]
    int     0x13
    jc      err_disk
    pop     di
    pop     edx
    pop     ecx
    pop     eax
    ret

err_edd:
    mov     al, 'A'
    jmp     halt_msg
err_disk:
    mov     al, 'D'
    jmp     halt_msg
err_bld:
    mov     al, 'B'
    jmp     halt_msg
halt_msg:
    mov     ah, 0x0E
    mov     bx, 0x0007
    int     0x10
halt_loop:
    hlt
    jmp     halt_loop

    .align 2
boot_drive:
    .byte   0
    .align 2
/* INT 13h Disk Address Packet. */
dap:
    .byte   0x10        /* DAP size */
    .byte   0           /* reserved */
dap_count:
    .word   0           /* sectors to transfer */
dap_off:
    .word   0           /* destination offset */
dap_seg:
    .word   0           /* destination segment */
dap_lba_lo:
    .long   0           /* starting LBA, low 32 bits */
dap_lba_hi:
    .long   0           /* starting LBA, high 32 bits */
