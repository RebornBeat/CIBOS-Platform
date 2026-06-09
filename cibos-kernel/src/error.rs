//! Kernel error type.

use core::fmt;
use shared::SharedError;

/// Result alias for kernel operations.
pub type KernelResult<T> = Result<T, KernelError>;

/// Errors originating in the CIBOS kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum KernelError {
    /// An error from the shared foundation layer.
    Shared(SharedError),
    /// A runtime error from the async runtime.
    Runtime(cibos_async_runtime::RuntimeError),
    /// Initialization failed during a specific phase.
    InitFailed {
        /// The phase that failed.
        phase: &'static str,
    },
    /// A referenced container does not exist.
    UnknownContainer,
    /// A referenced channel does not exist.
    UnknownChannel,
    /// A container resource limit was exceeded.
    LimitExceeded {
        /// The resource whose limit was hit.
        resource: &'static str,
    },
    /// An operation was attempted against an invalid or inconsistent state
    /// (e.g. mapping an already-mapped page, or an unaligned address).
    InvalidState {
        /// What was wrong.
        reason: &'static str,
    },
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KernelError::Shared(e) => write!(f, "{e}"),
            KernelError::Runtime(e) => write!(f, "{e}"),
            KernelError::InitFailed { phase } => write!(f, "kernel init failed at {phase}"),
            KernelError::UnknownContainer => write!(f, "unknown container"),
            KernelError::UnknownChannel => write!(f, "unknown channel"),
            KernelError::LimitExceeded { resource } => {
                write!(f, "resource limit exceeded: {resource}")
            }
            KernelError::InvalidState { reason } => {
                write!(f, "invalid state: {reason}")
            }
        }
    }
}

impl core::error::Error for KernelError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            KernelError::Shared(e) => Some(e),
            KernelError::Runtime(e) => Some(e),
            _ => None,
        }
    }
}

impl From<SharedError> for KernelError {
    fn from(e: SharedError) -> Self {
        KernelError::Shared(e)
    }
}

impl From<cibos_async_runtime::RuntimeError> for KernelError {
    fn from(e: cibos_async_runtime::RuntimeError) -> Self {
        KernelError::Runtime(e)
    }
}
