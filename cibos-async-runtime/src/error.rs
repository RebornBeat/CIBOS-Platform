//! Runtime error type.

use core::fmt;
use shared::SharedError;

/// Result alias for runtime operations.
pub type RuntimeResult<T> = Result<T, RuntimeError>;

/// Errors from the async runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimeError {
    /// An error from the shared foundation layer.
    Shared(SharedError),
    /// A lane was referenced that the executor does not know about.
    UnknownLane,
    /// The maximum lane count for the executor was exceeded.
    LaneLimitExceeded {
        /// The configured maximum.
        limit: usize,
    },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::Shared(e) => write!(f, "{e}"),
            RuntimeError::UnknownLane => write!(f, "unknown lane"),
            RuntimeError::LaneLimitExceeded { limit } => {
                write!(f, "lane limit exceeded (max {limit})")
            }
        }
    }
}

impl core::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            RuntimeError::Shared(e) => Some(e),
            _ => None,
        }
    }
}

impl From<SharedError> for RuntimeError {
    fn from(e: SharedError) -> Self {
        RuntimeError::Shared(e)
    }
}
