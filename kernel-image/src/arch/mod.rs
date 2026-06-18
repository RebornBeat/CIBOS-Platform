//! Architecture-specific console output and halt for the kernel image.
//!
//! Compiled only for `target_os = "none"`. Each backend provides `init_serial`,
//! `putc`, and `halt`. On x86_64 the console is dual: COM1 serial (the bring-up
//! capture target) and the VGA text console (on-screen output via `vga`). On
//! aarch64/riscv64 the console targets the QEMU `virt` serial defaults so the
//! kernel can prove liveness; an on-screen framebuffer console for those is a
//! later step.

#[cfg(target_arch = "x86_64")]
pub(crate) mod vga;
#[cfg(target_arch = "x86_64")]
pub mod gdt;
#[cfg(target_arch = "x86_64")]
pub mod idt;
#[cfg(target_arch = "x86_64")]
pub mod ata;
// Production NIC driver: always compiled on x86_64 and probed at boot, exactly
// like `ata`. virtio-net is a real, standardized interface (cloud VMs, bare-metal
// SR-IOV); QEMU is only the test harness. The `virtio-net-demo` feature controls
// verbose probe LOGGING, never whether this driver exists.
#[cfg(target_arch = "x86_64")]
pub mod virtio_net;
// Second production NIC driver: the Intel 82540EM (e1000), a ubiquitous physical
// NIC, so a non-virtio bare-metal box still has networking. Same NetDevice trait.
#[cfg(target_arch = "x86_64")]
pub mod e1000;
#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub mod paging;
#[cfg(all(target_arch = "x86_64", any(feature = "ring3-resume-demo", feature = "ring3-multilane-demo")))]
pub mod ring3_ctx;
#[cfg(target_arch = "x86_64")]
pub use x86_64::{
    halt, inb_port, init_pit, init_serial, inw, outb_port, outw, pic_eoi, pic_spurious, putc,
    read_keyboard_data, remap_pic, unmask_irq,
};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::{halt, init_serial, putc};

#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "riscv64")]
pub use riscv64::{halt, init_serial, putc};

#[cfg(target_arch = "x86")]
mod x86;
#[cfg(target_arch = "x86")]
pub use x86::{halt, init_serial, putc};
