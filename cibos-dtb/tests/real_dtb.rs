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
