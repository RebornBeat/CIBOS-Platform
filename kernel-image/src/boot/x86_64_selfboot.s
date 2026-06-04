// CIBOS kernel x86_64 self-boot entry.
//
// Used with the `self-boot` feature for standalone QEMU `-kernel` testing.
// Entered by a multiboot1 loader in 32-bit protected mode. Clears BSS,
// identity-maps the first 1 GiB, switches to 64-bit long mode, sets up the
// stack, and calls `kernel_entry`. The handoff is synthesized in Rust under
// self-boot, so the argument register is zeroed.

.section .multiboot, "a"
    .align 4
    .long 0x1BADB002                      // multiboot1 magic
    .long 0x00000000                      // flags
    .long -(0x1BADB002 + 0x00000000)      // checksum

.section .bss
    .align 4096
p4_table: .skip 4096
p3_table: .skip 4096
p2_table: .skip 4096

.section .rodata
    .align 8
gdt64:
    .quad 0                                               // null descriptor
    .quad (1 << 43) | (1 << 44) | (1 << 47) | (1 << 53)   // 64-bit code segment
gdt64_pointer:
    .word gdt64_pointer - gdt64 - 1
    .quad gdt64

.section .text.boot
.code32
.global _start
_start:
    mov esp, offset __stack_top

    // Clear BSS (page tables live here, so clear before populating them).
    mov edi, offset __bss_start
    mov ecx, offset __bss_end
    sub ecx, edi
    xor eax, eax
    rep stosb

    // Identity-map the first 1 GiB with 2 MiB huge pages.
    mov eax, offset p3_table
    or  eax, 0b11
    mov [p4_table], eax
    mov eax, offset p2_table
    or  eax, 0b11
    mov [p3_table], eax
    mov ecx, 0
.map_p2:
    mov eax, 0x200000
    mul ecx
    or  eax, 0b10000011
    mov [p2_table + ecx * 8], eax
    inc ecx
    cmp ecx, 512
    jne .map_p2

    mov eax, offset p4_table
    mov cr3, eax

    mov eax, cr4
    or  eax, 1 << 5                        // CR4.PAE
    mov cr4, eax

    mov ecx, 0xC0000080                    // EFER
    rdmsr
    or  eax, 1 << 8                        // LME
    wrmsr

    mov eax, cr0
    or  eax, 1 << 31                       // CR0.PG
    mov cr0, eax

    lgdt [gdt64_pointer]
    // Far jump into 64-bit code, hand-encoded ptr16:32 form.
    .byte 0xEA
    .long _start64
    .word 0x08

.code64
_start64:
    xor ax, ax
    mov ss, ax
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    lea rsp, [rip + __stack_top]

    // Self-boot: no handoff pointer; zero the first argument register.
    xor edi, edi
    call kernel_entry

.hang:
    cli
    hlt
    jmp .hang
