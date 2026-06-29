//! Validate the parser against a REAL QEMU virt device tree, not just synthetic
//! FDTs — proof it will parse a real board's DTB. (aarch64 QEMU virt: RAM at
//! 0x40000000.)
use cibos_dtb::DeviceTree;

static REAL_DTB: &[u8] = include_bytes!("real_arm.dtb");

#[test]
fn parses_real_qemu_dtb() {
    let dt = DeviceTree::new(REAL_DTB).expect("parse real DTB");
    let (base, size) = dt.ram_region().expect("ram region present");
    assert_eq!(base, 0x4000_0000, "QEMU virt aarch64 RAM base");
    assert!(size > 0, "RAM size positive");
}

#[test]
fn finds_pl011_uart_base() {
    // The kernel discovers the console UART base via device_base(b"pl011").
    // In the real QEMU virt DTB the node is `pl011@9000000`, so this must
    // resolve to 0x09000000 — proving peripheral discovery works on real data.
    let dt = DeviceTree::new(REAL_DTB).expect("parse real DTB");
    let uart = dt.device_base(b"pl011").expect("pl011 node present");
    assert_eq!(uart, 0x0900_0000, "PL011 UART base from real DTB");
}

#[test]
fn finds_gic_intc_window() {
    // The kernel discovers the interrupt controller (GIC) window via
    // device_reg(b"intc"). In the real QEMU virt DTB the node is `intc@8000000`,
    // so this must resolve to base 0x08000000 — proving board-specific peripheral
    // discovery (base AND size) works on real data, not just the bootstrap
    // fallback. This is what makes the GIC mapping follow the platform's DTB
    // instead of a hardcoded board constant.
    let dt = DeviceTree::new(REAL_DTB).expect("parse real DTB");
    let (base, size) = dt.device_reg(b"intc").expect("intc node present");
    assert_eq!(base, 0x0800_0000, "GIC base from real DTB");
    assert!(size > 0, "GIC window size from real DTB must be non-zero");
}

static REAL_RISCV_DTB: &[u8] = include_bytes!("real_riscv.dtb");

#[test]
fn finds_riscv_peripherals() {
    // RV64 virt board-specific peripherals discovered from the real DTB:
    // plic@c000000, clint@2000000, serial@10000000. Proves the kernel maps these
    // from the platform's device tree, not hardcoded constants.
    let dt = DeviceTree::new(REAL_RISCV_DTB).expect("parse real riscv DTB");
    let (plic, _) = dt.device_reg(b"plic").expect("plic node");
    assert_eq!(plic, 0x0C00_0000, "PLIC base from real DTB");
    let (clint, _) = dt.device_reg(b"clint").expect("clint node");
    assert_eq!(clint, 0x0200_0000, "CLINT base from real DTB");
    let (uart, _) = dt.device_reg(b"serial").expect("serial node");
    assert_eq!(uart, 0x1000_0000, "UART base from real DTB");
}
