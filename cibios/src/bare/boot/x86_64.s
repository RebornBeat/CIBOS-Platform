// CIBIOS x86_64 boot entry.
//
// Entered by a multiboot1 loader (e.g. QEMU `-kernel`) in 32-bit protected
// mode. Responsibilities: clear BSS, identity-map the first 1 GiB, switch to
// 64-bit long mode, set up the stack, and call the Rust entry `cibios_entry`.
//
// QEMU BRING-UP NOTE: this is the path to validate in QEMU. The multiboot
// header and the 32->64 transition below are standard; if you chainload from a
// stage that already enters long mode, replace _start with a thin 64-bit stub.

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

.section .data
    .align 8
.global multiboot_info_ptr
multiboot_info_ptr: .quad 0

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
    // Set up the boot stack early.
    mov esp, offset __stack_top

    // Save the multiboot information pointer (passed in ebx by the loader)
    // before any register is clobbered. Lives in .data so the BSS clear below
    // does not erase it.
    mov dword ptr [multiboot_info_ptr], ebx

    // Clear BSS first -- the page tables live in BSS, so this must happen
    // before we populate them.
    mov edi, offset __bss_start
    mov ecx, offset __bss_end
    sub ecx, edi
    xor eax, eax
    rep stosb

    // Identity-map the first 1 GiB using 2 MiB huge pages.
    // p4[0] -> p3
    mov eax, offset p3_table
    or  eax, 0b11                          // present + writable
    mov [p4_table], eax
    // p3[0] -> p2
    mov eax, offset p2_table
    or  eax, 0b11
    mov [p3_table], eax
    // p2[i] -> i * 2 MiB, huge
    mov ecx, 0
.map_p2:
    mov eax, 0x200000
    mul ecx                                // edx:eax = 2MiB * ecx
    or  eax, 0b10000011                    // present + writable + huge
    mov [p2_table + ecx * 8], eax
    inc ecx
    cmp ecx, 512
    jne .map_p2

    // Load CR3 with the top-level table.
    mov eax, offset p4_table
    mov cr3, eax

    // Enable PAE (CR4.PAE = bit 5).
    mov eax, cr4
    or  eax, 1 << 5
    mov cr4, eax

    // Set EFER.LME (long mode enable) via MSR 0xC0000080.
    mov ecx, 0xC0000080
    rdmsr
    or  eax, 1 << 8
    wrmsr

    // Enable paging (CR0.PG = bit 31).
    mov eax, cr0
    or  eax, 1 << 31
    mov cr0, eax

    // Load the 64-bit GDT and far-jump into 64-bit code.
    lgdt [gdt64_pointer]
    // Far jump `jmp 0x08:_start64`, hand-encoded as the ptr16:32 form
    // (opcode 0xEA, 32-bit offset, 16-bit selector) because the integrated
    // assembler does not accept the `selector:label` mnemonic form.
    .byte 0xEA
    .long _start64
    .word 0x08

.code64
_start64:
    // Reset data segment registers.
    xor ax, ax
    mov ss, ax
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    // Re-establish the stack pointer in 64-bit mode (RIP-relative to avoid an
    // absolute relocation the linker rejects in this model).
    lea rsp, [rip + __stack_top]

    // Hand off to Rust. cibios_entry is `extern "C" fn() -> !`.
    call cibios_entry

.hang:
    cli
    hlt
    jmp .hang
