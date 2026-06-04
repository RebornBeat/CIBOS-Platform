//! SDK error type.

use std::fmt;

/// Result alias for SDK operations.
pub type SdkResult<T> = Result<T, SdkError>;

/// Errors surfaced to applications through the SDK.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SdkError {
    /// An error from the shared foundation layer.
    Shared(shared::SharedError),
    /// An error from the kernel.
    Kernel(cibos_kernel::KernelError),
    /// A channel operation failed.
    Channel(shared::types::error::ProtocolError),
    /// The application exceeded a resource limit.
    LimitExceeded {
        /// The resource whose limit was hit.
        resource: &'static str,
    },
}

impl fmt::Display for SdkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SdkError::Shared(e) => write!(f, "{e}"),
            SdkError::Kernel(e) => write!(f, "{e}"),
            SdkError::Channel(e) => write!(f, "channel error: {e}"),
            SdkError::LimitExceeded { resource } => {
                write!(f, "resource limit exceeded: {resource}")
            }
        }
    }
}

impl std::error::Error for SdkError {}

impl From<shared::SharedError> for SdkError {
    fn from(e: shared::SharedError) -> Self {
        SdkError::Shared(e)
    }
}

impl From<cibos_kernel::KernelError> for SdkError {
    fn from(e: cibos_kernel::KernelError) -> Self {
        SdkError::Kernel(e)
    }
}

impl From<shared::types::error::ProtocolError> for SdkError {
    fn from(e: shared::types::error::ProtocolError) -> Self {
        SdkError::Channel(e)
    }
}
