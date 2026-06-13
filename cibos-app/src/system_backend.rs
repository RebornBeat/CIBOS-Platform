//! The on-kernel [`ShellSystem`] backend.
//!
//! [`SyscallSystem`] implements the shared [`cibos_console::ShellSystem`] /
//! [`cibos_console::ShellFs`] traits on top of this runtime's syscall
//! primitives, so the *real* `shell::dispatch` (and any other line-oriented app
//! written to those traits) runs unchanged in ring 3: filesystem operations go
//! through the `Fs*` syscalls, `now_nanos` through `Now`, and the resource
//! limits are the fixed grant this process runs under.
//!
//! Paths are UTF-8 `&str` at the trait boundary (matching the host SDK); this
//! backend passes their bytes to the kernel, which resolves them against the
//! mounted root filesystem (CIBOSFS).

use alloc::string::String;
use alloc::vec::Vec;
use cibos_console::{ShellFs, ShellSystem};
use shared::ResourceLimits;

/// Largest single file this backend will read through [`ShellFs::read`]. Shell
/// workloads (config, notes, small data) fit comfortably; the cap bounds the
/// scratch buffer so a ring-3 app cannot be asked to allocate without limit.
const MAX_READ_BYTES: usize = 64 * 1024;

/// A [`ShellSystem`] backed by the kernel syscall interface.
///
/// Zero-sized: all state lives in the kernel. The resource limits it reports are
/// the fixed grant configured at construction (a real system would receive these
/// from the kernel at process start; until that is wired, a conservative default
/// is used).
#[derive(Debug, Clone, Copy)]
pub struct SyscallSystem {
    limits: ResourceLimits,
}

impl SyscallSystem {
    /// A new syscall-backed system with the given resource limits.
    #[must_use]
    pub const fn new(limits: ResourceLimits) -> Self {
        SyscallSystem { limits }
    }
}

impl Default for SyscallSystem {
    fn default() -> Self {
        // A conservative default grant for an interactive shell process.
        SyscallSystem::new(ResourceLimits {
            memory_bytes: 16 * 1024 * 1024,
            max_lanes: 1,
            max_channels: 0,
            max_message_bytes: 0,
            max_channel_buffer: 0,
        })
    }
}

/// The filesystem handle [`SyscallSystem`] hands out (zero-sized; all calls are
/// syscalls).
#[derive(Debug, Default, Clone, Copy)]
pub struct SyscallFs;

impl ShellFs for SyscallFs {
    fn write(&self, path: &str, data: &[u8]) -> bool {
        crate::fs::write(path.as_bytes(), data).is_ok()
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let mut buf = alloc::vec![0u8; MAX_READ_BYTES];
        match crate::fs::read_into(path.as_bytes(), &mut buf) {
            Ok(n) => {
                buf.truncate(n);
                Some(buf)
            }
            Err(_) => None,
        }
    }

    fn list(&self, path: &str) -> Vec<String> {
        crate::fs::list(path.as_bytes()).unwrap_or_default()
    }

    fn delete(&self, path: &str) -> bool {
        crate::fs::delete(path.as_bytes()).is_ok()
    }
}

impl ShellSystem for SyscallSystem {
    type Fs = SyscallFs;

    fn filesystem(&self) -> SyscallFs {
        SyscallFs
    }

    fn now_nanos(&self) -> u64 {
        crate::rand::now_nanos()
    }

    fn resource_limits(&self) -> ResourceLimits {
        self.limits
    }
}
