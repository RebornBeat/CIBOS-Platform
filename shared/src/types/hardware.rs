//! # Hardware Abstraction Types
//!
//! Cross-cutting descriptions of the hardware a CIBIOS/CIBOS system runs on.
//!
//! Several of these types are embedded in the firmware→kernel handoff structure
//! (see `protocols::handoff`). Because the handoff is written by one binary
//! (CIBIOS) and read by another (CIBOS), every type that crosses that boundary
//! has a fixed, explicitly-tagged representation: enums are `#[repr(u32)]` with
//! a hand-written [`TryFrom<u32>`] so the receiver can reject malformed
//! discriminants rather than transmute blindly. Capability sets use `bitflags`
//! over a `u32`, which has an equally stable layout.
//!
//! Types here are pure data. They contain no behavior beyond construction,
//! validation, and conversion, which keeps them safe to share across the
//! `no_std`/`std` boundary unchanged.

use crate::types::error::{HardwareError, SerializationError};
use bitflags::bitflags;

/// Processor architecture the system is running on.
///
/// `#[repr(u32)]` with explicit discriminants because this value is written
/// into the handoff record by CIBIOS and read back by CIBOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum ProcessorArchitecture {
    /// Intel/AMD 64-bit.
    X86_64 = 1,
    /// ARM 64-bit.
    AArch64 = 2,
    /// Intel/AMD 32-bit.
    X86 = 3,
    /// RISC-V 64-bit.
    RiscV64 = 4,
}

impl ProcessorArchitecture {
    /// Returns the architecture this binary was compiled for, or `None` if the
    /// compile target is not one of the supported architectures.
    #[must_use]
    pub const fn current() -> Option<Self> {
        #[cfg(target_arch = "x86_64")]
        {
            Some(ProcessorArchitecture::X86_64)
        }
        #[cfg(target_arch = "aarch64")]
        {
            Some(ProcessorArchitecture::AArch64)
        }
        #[cfg(target_arch = "x86")]
        {
            Some(ProcessorArchitecture::X86)
        }
        #[cfg(target_arch = "riscv64")]
        {
            Some(ProcessorArchitecture::RiscV64)
        }
        #[cfg(not(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "x86",
            target_arch = "riscv64"
        )))]
        {
            None
        }
    }

    /// The raw `u32` discriminant, for writing into handoff/wire structures.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

impl TryFrom<u32> for ProcessorArchitecture {
    type Error = SerializationError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(ProcessorArchitecture::X86_64),
            2 => Ok(ProcessorArchitecture::AArch64),
            3 => Ok(ProcessorArchitecture::X86),
            4 => Ok(ProcessorArchitecture::RiscV64),
            _ => Err(SerializationError::InvalidValue {
                field: "ProcessorArchitecture",
            }),
        }
    }
}

/// Broad class of device the system is deployed on.
///
/// Drives platform-variant selection and default capability sets. `#[repr(u32)]`
/// because it appears in the handoff record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum HardwarePlatform {
    /// Fixed desktop workstation.
    Desktop = 1,
    /// Portable laptop.
    Laptop = 2,
    /// Headless or rack server.
    Server = 3,
    /// Smartphone.
    Mobile = 4,
    /// Tablet.
    Tablet = 5,
    /// Embedded / single-board device.
    Embedded = 6,
}

impl HardwarePlatform {
    /// The raw `u32` discriminant, for writing into handoff/wire structures.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Whether this platform class is expected to have a touch-first interface.
    #[must_use]
    pub const fn is_touch_first(self) -> bool {
        matches!(self, HardwarePlatform::Mobile | HardwarePlatform::Tablet)
    }
}

impl TryFrom<u32> for HardwarePlatform {
    type Error = SerializationError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(HardwarePlatform::Desktop),
            2 => Ok(HardwarePlatform::Laptop),
            3 => Ok(HardwarePlatform::Server),
            4 => Ok(HardwarePlatform::Mobile),
            5 => Ok(HardwarePlatform::Tablet),
            6 => Ok(HardwarePlatform::Embedded),
            _ => Err(SerializationError::InvalidValue {
                field: "HardwarePlatform",
            }),
        }
    }
}

bitflags! {
    /// Optional hardware security features the platform exposes.
    ///
    /// These are *capabilities*, not policy. Whether a given capability is
    /// actually used is decided by build features and configuration — for
    /// example, vendor virtualization extensions are never enabled by default.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SecurityCapabilities: u32 {
        /// A hardware random number generator is present (RDRAND/RNDR/SEED).
        const HARDWARE_RNG       = 1 << 0;
        /// Intel VT-x virtualization extensions are available.
        const VTX                = 1 << 1;
        /// AMD SVM virtualization extensions are available.
        const SVM                = 1 << 2;
        /// ARM TrustZone is available.
        const TRUSTZONE          = 1 << 3;
        /// Hardware memory encryption is available (e.g. SME/TME).
        const MEMORY_ENCRYPTION  = 1 << 4;
        /// A measured/secure boot facility is available.
        const SECURE_BOOT        = 1 << 5;
        /// An IOMMU is present for DMA isolation.
        const IOMMU              = 1 << 6;
    }
}

bitflags! {
    /// Input modalities the platform supports.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InputCapabilities: u32 {
        /// A physical or virtual keyboard is available.
        const KEYBOARD = 1 << 0;
        /// A pointing device (mouse/trackpad) is available.
        const POINTER  = 1 << 1;
        /// A touchscreen is available.
        const TOUCH    = 1 << 2;
        /// Hardware buttons (power/volume) are available.
        const BUTTONS  = 1 << 3;
    }
}

bitflags! {
    /// Sensor hardware the platform exposes (primarily mobile).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SensorCapabilities: u32 {
        /// Camera sensor.
        const CAMERA        = 1 << 0;
        /// Microphone.
        const MICROPHONE    = 1 << 1;
        /// GPS / GNSS receiver.
        const GPS           = 1 << 2;
        /// Accelerometer.
        const ACCELEROMETER = 1 << 3;
        /// Gyroscope.
        const GYROSCOPE     = 1 << 4;
        /// Barometer.
        const BAROMETER     = 1 << 5;
        /// Proximity sensor.
        const PROXIMITY     = 1 << 6;
    }
}

bitflags! {
    /// Connectivity hardware the platform exposes.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct NetworkCapabilities: u32 {
        /// Wired Ethernet.
        const ETHERNET  = 1 << 0;
        /// Wi-Fi.
        const WIFI      = 1 << 1;
        /// Cellular modem.
        const CELLULAR  = 1 << 2;
        /// Bluetooth.
        const BLUETOOTH = 1 << 3;
    }
}

/// Physical/logical core topology, as established by firmware and reported to
/// the kernel through the handoff record.
///
/// The number of simultaneous execution contexts the HIP scheduler can use is
/// `logical_cores`, which equals `physical_cores * smt_factor` when SMT is
/// enabled and `physical_cores` when it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct CoreTopology {
    /// Number of physical cores present.
    pub physical_cores: u32,
    /// Number of logical cores exposed (after SMT configuration).
    pub logical_cores: u32,
    /// Whether SMT is enabled.
    pub smt_enabled: bool,
}

impl CoreTopology {
    /// Construct a topology, validating that logical core count is consistent
    /// with the physical count and SMT state.
    ///
    /// # Errors
    ///
    /// Returns [`HardwareError::InitializationFailed`] if the counts are
    /// inconsistent (zero physical cores, or fewer logical than physical).
    pub fn new(
        physical_cores: u32,
        logical_cores: u32,
        smt_enabled: bool,
    ) -> Result<Self, HardwareError> {
        if physical_cores == 0 {
            return Err(HardwareError::InitializationFailed {
                subsystem: "core_topology",
                code: -1,
            });
        }
        if logical_cores < physical_cores {
            return Err(HardwareError::InitializationFailed {
                subsystem: "core_topology",
                code: -2,
            });
        }
        if !smt_enabled && logical_cores != physical_cores {
            return Err(HardwareError::InitializationFailed {
                subsystem: "core_topology",
                code: -3,
            });
        }
        Ok(Self {
            physical_cores,
            logical_cores,
            smt_enabled,
        })
    }

    /// The number of simultaneous execution contexts the scheduler may use.
    /// This is the `C` in the HIP "N ≤ C" dispatch decision.
    #[must_use]
    pub const fn execution_contexts(self) -> u32 {
        self.logical_cores
    }
}

/// A contiguous region of physical memory with a defined purpose.
///
/// `#[repr(C)]` because arrays of these are embedded in the handoff memory map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct MemoryRegion {
    /// Physical start address of the region.
    pub base: u64,
    /// Length of the region in bytes.
    pub length: u64,
    /// What the region is used for.
    pub kind: MemoryRegionKind,
}

impl MemoryRegion {
    /// The exclusive end address of the region (`base + length`).
    #[must_use]
    pub const fn end(self) -> u64 {
        self.base + self.length
    }

    /// Whether this region contains the given physical address.
    #[must_use]
    pub const fn contains(self, addr: u64) -> bool {
        addr >= self.base && addr < self.end()
    }
}

/// The purpose of a [`MemoryRegion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MemoryRegionKind {
    /// General-purpose RAM available for allocation.
    Usable = 1,
    /// Memory reserved by firmware; the kernel must not touch it.
    FirmwareReserved = 2,
    /// Memory reserved for isolated lane execution contexts.
    LaneReserved = 3,
    /// Memory-mapped device I/O.
    DeviceMmio = 4,
    /// Defective or unusable memory.
    Bad = 5,
}

impl MemoryRegionKind {
    /// The raw `u32` discriminant, for writing into handoff/wire structures.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

impl TryFrom<u32> for MemoryRegionKind {
    type Error = SerializationError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(MemoryRegionKind::Usable),
            2 => Ok(MemoryRegionKind::FirmwareReserved),
            3 => Ok(MemoryRegionKind::LaneReserved),
            4 => Ok(MemoryRegionKind::DeviceMmio),
            5 => Ok(MemoryRegionKind::Bad),
            _ => Err(SerializationError::InvalidValue {
                field: "MemoryRegionKind",
            }),
        }
    }
}

/// A complete description of a system's hardware, assembled by firmware during
/// detection and used by both the firmware UI and (via handoff) the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardwareProfile {
    /// Processor architecture.
    pub architecture: ProcessorArchitecture,
    /// Device class.
    pub platform: HardwarePlatform,
    /// Core topology and SMT state.
    pub topology: CoreTopology,
    /// Total usable RAM in bytes.
    pub total_memory: u64,
    /// Security features the hardware exposes.
    pub security: SecurityCapabilities,
    /// Input modalities present.
    pub input: InputCapabilities,
    /// Sensors present.
    pub sensors: SensorCapabilities,
    /// Connectivity present.
    pub network: NetworkCapabilities,
}
