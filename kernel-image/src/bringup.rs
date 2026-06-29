//! The per-architecture bring-up contract.
//!
//! This is the executable expression of the canonical CIBOS bring-up sequence.
//! `kernel_entry` runs the portable phases (heap, handoff, scheduler core)
//! directly, then drives the PER-ARCH phases through [`ArchBringUp`] in a fixed
//! canonical order — identical control flow on every architecture, with zero
//! `target_arch` branching in the boot flow itself.
//!
//! x86_64 is the reference implementation: its impl wires the existing, verified
//! bring-up functions unchanged. Other architectures implement the SAME trait;
//! a phase an arch has not built yet returns [`PhaseStatus::Skipped`] honestly,
//! and is filled in later by implementing that one method — never by adding a new
//! `cfg` block. This makes every architecture follow the identical sequence by
//! construction, so they stay aligned to the x86_64 reference as they grow.

use shared::protocols::handoff::HandoffData;

/// The per-architecture paging hooks that the (portable) MMU bring-up
/// orchestration needs. Everything else about building and installing the
/// kernel's page tables is identical across architectures and lives in the
/// shared `bring_up_mmu` orchestration; only these few hooks differ.
///
/// This is the deeper application of the same no-drift principle as
/// [`ArchBringUp`]: one shared orchestration, a tiny per-arch surface. An arch
/// supplies its page-table entry encoder (via the portable
/// [`cibos_kernel::paging::PageTableEncoder`]) plus the register operations to
/// enable table features, install a root, and read the active root, plus the
/// device-MMIO ranges that must be identity-mapped (x86 PCI hole / ARM GIC+UART /
/// RISC-V PLIC+UART).
pub trait ArchPaging {
    /// The architecture's page-table entry encoder.
    type Encoder: cibos_kernel::paging::PageTableEncoder;

    /// How many bytes above the RAM base to reserve for the kernel's own image,
    /// heap, and stack, so the frame allocator never hands those out for page
    /// tables. This is a per-arch constant (it depends on the kernel's load
    /// offset within RAM + heap size), NOT a platform address — the actual
    /// reservation watermark is computed by the orchestration as
    /// `ram_base + kernel_span()`, so it is correct wherever the board places RAM.
    fn kernel_span() -> u64;

    /// A floor for how many bytes of low physical address space to identity-map,
    /// regardless of where RAM is. The orchestration maps
    /// `max(min_identity_map_bytes(), ram_base + ram_size)` so that (a) all of
    /// real RAM is covered wherever it sits, and (b) at least this much low space
    /// (covering low device MMIO) is always mapped. On the PC, RAM starts near 0
    /// so this floor is the whole map; on boards with high RAM it guarantees the
    /// low device region is mapped even though RAM is elsewhere.
    fn min_identity_map_bytes() -> u64;

    /// Device-MMIO ranges `(base, length)` to identity-map (kernel-rw, non-exec)
    /// so MMIO-BAR drivers can reach their registers. May be empty.
    fn mmio_identity_ranges() -> &'static [(u64, u64)];

    /// Enable any page-table entry features the arch needs before installing
    /// tables that use them (x86: EFER.NXE for the NX bit; others as required).
    ///
    /// # Safety
    /// Modifies control registers/MSRs; call once during single-threaded
    /// bring-up before installing tables.
    unsafe fn enable_table_features();

    /// Install `root` as the active page-table root (x86: CR3; aarch64: TTBR0_EL1
    /// + enable SCTLR_EL1.M; riscv64: satp). Execution continuing past this proves
    /// the tables are valid hardware tables.
    ///
    /// # Safety
    /// `root` must map at least all memory the kernel currently executes from and
    /// its stack, or the next fetch faults.
    unsafe fn install(root: cibos_kernel::PhysFrame);

    /// Read the active page-table root's physical address (x86: CR3; etc.).
    fn current_root() -> u64;
}

/// The outcome of one bring-up phase.
///
/// Not every architecture constructs every variant: a fully-built arch (x86_64)
/// returns `Done`/`Failed`, while an arch still growing its backend returns
/// `Skipped` for pending phases. All variants are part of the contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum PhaseStatus {
    /// The phase completed successfully.
    Done,
    /// The phase is not applicable or not yet implemented on this arch. The
    /// `&'static str` is an honest reason (e.g. "pending: MMU encoder").
    Skipped(&'static str),
    /// The phase ran but failed. The `&'static str` is a short cause.
    Failed(&'static str),
}

impl PhaseStatus {
    /// A human-readable label for boot logging.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            PhaseStatus::Done => "ok",
            PhaseStatus::Skipped(_) => "skipped",
            PhaseStatus::Failed(_) => "FAILED",
        }
    }
}

/// The per-architecture bring-up contract. Each method is one phase of the
/// canonical sequence, in the order `kernel_entry` calls them. The portable
/// phases (heap, handoff acceptance, scheduler core) are NOT here — they run
/// directly in `kernel_entry` and already work on every arch.
///
/// The order `kernel_entry` uses:
///   - `early_traps`  — install fault/trap vectors + enable FP (faults visible)
///   (heap / handoff / scheduler core run here, portable, on every arch)
///   - `seed_entropy` — seed the CSPRNG from the handoff entropy seed
///   - `mount_root_fs`— bring up block storage and mount the root FS (pre-MMU;
///     the ATA driver is port-I/O and needs no MMU)
///   - `bring_up_mmu` — build + install the kernel page tables; on x86_64 this
///     phase also OWNS the frame allocator and, within its scope, probes the NIC
///     and drops to ring 3 (those sub-phases borrow the allocator it owns, so
///     they are not separable top-level calls — they live inside this phase)
///   - `verify_storage`— read-back proof that block I/O works against real hw
///
/// An arch that has not built a phase returns [`PhaseStatus::Skipped`] with an
/// honest reason; it is filled in later by implementing that one method, never
/// by adding a `cfg` block to `kernel_entry`.
pub trait ArchBringUp {
    /// Install fault/trap vectors and enable the FPU/SIMD as the arch requires,
    /// so any subsequent fault is REPORTED rather than silent. Called as early as
    /// possible. Infallible by contract (a failure here is fatal in place).
    fn early_traps(&self);

    /// Seed the kernel CSPRNG (backs the `get_random` syscall) from the firmware
    /// entropy seed. The RNG is portable; this hook lets an arch gate it if its
    /// path is not ready.
    fn seed_entropy(&self, seed: &[u8]) -> PhaseStatus;

    /// Probe block storage and mount the root filesystem. Runs before the MMU
    /// phase (the ATA driver is port-I/O and needs no page tables).
    fn mount_root_fs(&self) -> PhaseStatus;

    /// Build the kernel's own page tables via the portable model + the arch
    /// encoder and install them. On x86_64 this phase also owns the frame
    /// allocator and, within its scope, probes the NIC and drops to ring 3.
    fn bring_up_mmu(&self, handoff: &HandoffData) -> PhaseStatus;

    /// Read back the boot medium to prove block I/O against real hardware.
    fn verify_storage(&self) -> PhaseStatus;
}
