//! # Firmware Error Type
//!
//! [`FirmwareError`] is CIBIOS's local error vocabulary. It adds the failure
//! cases specific to the firmware phase — image parsing, the boot sequence,
//! handoff construction — and wraps the cross-cutting [`SharedError`] for
//! everything inherited from the foundation layer. The `From<SharedError>`
//! conversion lets `?` lift shared errors into firmware errors transparently.

use core::fmt;
use shared::SharedError;

/// Result alias for firmware operations.
pub type FirmwareResult<T> = Result<T, FirmwareError>;

/// Errors that can occur during the CIBIOS firmware phase.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FirmwareError {
    /// An error originating in the shared foundation layer.
    Shared(SharedError),
    /// The boot sequence could not complete.
    BootFailure {
        /// The phase of boot that failed.
        phase: &'static str,
    },
    /// The CIBOS image could not be located on boot media.
    ImageNotFound,
    /// The CIBOS image header or structure was malformed.
    MalformedImage {
        /// Description of the malformation.
        detail: &'static str,
    },
    /// A component's content hash did not match its descriptor.
    ComponentHashMismatch {
        /// Index of the offending component.
        index: u32,
    },
    /// The image targets an architecture other than the running one.
    ArchitectureMismatch {
        /// Architecture the image was built for.
        image: u32,
        /// Architecture currently running.
        running: u32,
    },
    /// Control could not be transferred to the kernel.
    HandoffFailure {
        /// Description of the failure.
        detail: &'static str,
    },
}

impl fmt::Display for FirmwareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FirmwareError::Shared(e) => write!(f, "{e}"),
            FirmwareError::BootFailure { phase } => write!(f, "boot failure during {phase}"),
            FirmwareError::ImageNotFound => write!(f, "CIBOS image not found on boot media"),
            FirmwareError::MalformedImage { detail } => {
                write!(f, "malformed CIBOS image: {detail}")
            }
            FirmwareError::ComponentHashMismatch { index } => {
                write!(f, "component {index} failed hash verification")
            }
            FirmwareError::ArchitectureMismatch { image, running } => write!(
                f,
                "image architecture {image} does not match running architecture {running}"
            ),
            FirmwareError::HandoffFailure { detail } => {
                write!(f, "handoff to kernel failed: {detail}")
            }
        }
    }
}

impl core::error::Error for FirmwareError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            FirmwareError::Shared(e) => Some(e),
            _ => None,
        }
    }
}

impl From<SharedError> for FirmwareError {
    fn from(e: SharedError) -> Self {
        FirmwareError::Shared(e)
    }
}
