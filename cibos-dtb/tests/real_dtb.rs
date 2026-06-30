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

#[test]
fn detects_dma_coherent_property() {
    // node_has_prop must detect the dma-coherent boolean on the ARM virtio-mmio
    // node (present) and its ABSENCE on the RISC-V virtio-mmio node — a real
    // platform difference the block driver must honor (barrier-only DMA is only
    // safe when the platform guarantees coherency).
    let arm = DeviceTree::new(REAL_DTB).expect("parse ARM DTB");
    assert!(
        arm.node_has_prop(b"virtio_mmio", b"dma-coherent"),
        "ARM virtio-mmio is dma-coherent"
    );
    let rv = DeviceTree::new(REAL_RISCV_DTB).expect("parse RISC-V DTB");
    assert!(
        !rv.node_has_prop(b"virtio_mmio", b"dma-coherent"),
        "RISC-V virtio-mmio is NOT dma-coherent"
    );
}

#[test]
fn device_reg_lowest_picks_lowest_base() {
    // The RISC-V virt DTB lists virtio_mmio@ nodes in DESCENDING address order
    // (@10008000 first). device_reg_lowest must return the LOWEST base
    // (0x10001000), not the first node — a driver walking the slot array upward
    // from the base must start at the bottom or it runs past the array and faults.
    let rv = DeviceTree::new(REAL_RISCV_DTB).expect("parse RISC-V DTB");
    let (base, _) = rv.device_reg_lowest(b"virtio_mmio").expect("virtio_mmio node");
    assert_eq!(base, 0x1000_1000, "lowest virtio_mmio base on RISC-V virt");
    // And the count must be the real number of slots (8 on riscv64 virt).
    assert_eq!(rv.count_nodes(b"virtio_mmio"), 8, "riscv64 virt virtio_mmio slot count");
}

#[test]
fn walkers_handle_truncated_blob() {
    // A truncated DTB must make the structure walkers return safely (0 / None /
    // false), never panic or loop — real firmware could pass a damaged blob.
    // Truncate the valid RISC-V DTB partway through its structure block.
    let full = REAL_RISCV_DTB;
    let cut = &full[..full.len() / 2];
    // DeviceTree::new may reject it; if it parses, the walkers must be safe.
    if let Ok(dt) = DeviceTree::new(cut) {
        let _ = dt.count_nodes(b"virtio_mmio");
        let _ = dt.device_reg_lowest(b"virtio_mmio");
        let _ = dt.find_prop_u32(b"riscv,cbom-block-size");
        let _ = dt.node_has_prop(b"virtio_mmio", b"dma-coherent");
    }
    // Also a blob that is just the header with a bogus struct size: must not hang.
    let mut bad = full.to_vec();
    // Corrupt totalsize/struct fields lightly by zeroing the tail.
    for b in bad.iter_mut().skip(full.len() / 2) {
        *b = 0;
    }
    if let Ok(dt) = DeviceTree::new(&bad) {
        let _ = dt.count_nodes(b"virtio_mmio");
        let _ = dt.device_reg_lowest(b"virtio_mmio");
    }
}
