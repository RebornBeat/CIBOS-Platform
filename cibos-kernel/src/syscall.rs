//! # Syscall Dispatch
//!
//! The portable kernel-side handler for the [`shared::protocols::syscall`] ABI.
//! The architecture trap entry decodes the hardware registers into a
//! [`SyscallRequest`] and calls [`dispatch`]; everything here is
//! architecture-neutral and host-tested.
//!
//! ## Memory safety across the boundary
//!
//! A user pointer argument is an address in the *calling boundary's* address
//! space, which is not the kernel's. The dispatcher never dereferences a user
//! pointer directly; it asks the caller-supplied [`UserMemory`] to translate and
//! copy bytes, so the (architecture-specific, `unsafe`) act of reading another
//! address space's memory stays in one audited place and every access is
//! bounds-checked against that boundary's mappings.

use shared::protocols::syscall::{Syscall, SyscallError};
use shared::BoundaryId;

/// Maximum bytes a single `log` may emit, to bound kernel work per call.
pub const MAX_LOG_LEN: usize = 4096;

/// A decoded syscall request: the number and up to three argument words, plus
/// the boundary that issued it.
#[derive(Debug, Clone, Copy)]
pub struct SyscallRequest {
    /// Raw syscall number (from the number register).
    pub number: u64,
    /// Argument 0.
    pub arg0: u64,
    /// Argument 1.
    pub arg1: u64,
    /// Argument 2.
    pub arg2: u64,
    /// The boundary that issued the trap.
    pub boundary: BoundaryId,
}

/// What the dispatcher decided should happen after handling a syscall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallOutcome {
    /// Return this value to the caller (placed in the return register).
    Return(i64),
    /// The caller asked to exit with this code; the kernel tears down the
    /// boundary rather than returning to user code.
    Exit(u64),
    /// The caller yielded; the kernel should reschedule before returning 0.
    Yield,
}

/// Access to the calling boundary's memory and to kernel services the
/// dispatcher needs. Implemented by the arch/kernel layer; modelled in tests.
pub trait SyscallEnv {
    /// Copy `len` bytes from the user virtual address `ptr` (in `boundary`'s
    /// space) into `out`. Returns `Err(BadAddress)` if the range is not fully
    /// mapped and readable in that boundary. `out.len()` bounds the copy.
    fn copy_from_user(
        &self,
        boundary: BoundaryId,
        ptr: u64,
        len: usize,
        out: &mut [u8],
    ) -> Result<(), SyscallError>;

    /// Write already-validated UTF-8-ish console bytes to the kernel console.
    fn console_write(&self, bytes: &[u8]);

    /// Monotonic nanoseconds since boot (for `now`).
    fn now_nanos(&self) -> u64;
}

/// Handle one syscall request against `env`, returning the outcome.
///
/// Pure and synchronous: it validates arguments, performs the primitive, and
/// reports what the trap glue should do. It never panics on bad user input —
/// invalid arguments become [`SyscallError`] return values.
pub fn dispatch<E: SyscallEnv>(req: &SyscallRequest, env: &E) -> SyscallOutcome {
    let Some(call) = Syscall::from_number(req.number) else {
        return SyscallOutcome::Return(SyscallError::NoSuchCall.as_return());
    };

    match call {
        Syscall::Log => {
            let ptr = req.arg0;
            let len = req.arg1 as usize;
            if len > MAX_LOG_LEN {
                return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
            }
            // Copy into a fixed kernel buffer (bounded by MAX_LOG_LEN), then
            // write. No allocation, no trust in the user length beyond the cap.
            let mut buf = [0u8; MAX_LOG_LEN];
            match env.copy_from_user(req.boundary, ptr, len, &mut buf[..len]) {
                Ok(()) => {
                    env.console_write(&buf[..len]);
                    SyscallOutcome::Return(0)
                }
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::Exit => SyscallOutcome::Exit(req.arg0),
        Syscall::Yield => SyscallOutcome::Yield,
        Syscall::Now => {
            // Monotonic nanoseconds fit in i64 for ~292 years; clamp defensively.
            let ns = env.now_nanos();
            SyscallOutcome::Return((ns & (i64::MAX as u64)) as i64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;

    struct MockEnv {
        // A single mapped region [base, base+data.len()) in the test boundary.
        base: u64,
        data: alloc::vec::Vec<u8>,
        written: RefCell<alloc::vec::Vec<u8>>,
        clock: u64,
    }

    impl SyscallEnv for MockEnv {
        fn copy_from_user(
            &self,
            _boundary: BoundaryId,
            ptr: u64,
            len: usize,
            out: &mut [u8],
        ) -> Result<(), SyscallError> {
            let start = ptr
                .checked_sub(self.base)
                .ok_or(SyscallError::BadAddress)? as usize;
            let end = start.checked_add(len).ok_or(SyscallError::InvalidArgument)?;
            if end > self.data.len() {
                return Err(SyscallError::BadAddress);
            }
            out[..len].copy_from_slice(&self.data[start..end]);
            Ok(())
        }
        fn console_write(&self, bytes: &[u8]) {
            self.written.borrow_mut().extend_from_slice(bytes);
        }
        fn now_nanos(&self) -> u64 {
            self.clock
        }
    }

    fn env_with(text: &str) -> MockEnv {
        MockEnv {
            base: 0x4000_0000,
            data: text.as_bytes().to_vec(),
            written: RefCell::new(alloc::vec::Vec::new()),
            clock: 123_456_789,
        }
    }

    fn req(number: u64, a0: u64, a1: u64) -> SyscallRequest {
        SyscallRequest {
            number,
            arg0: a0,
            arg1: a1,
            arg2: 0,
            boundary: BoundaryId::new(1),
        }
    }

    #[test]
    fn log_writes_user_string() {
        let env = env_with("hello kernel");
        let r = req(Syscall::Log.number(), env.base, 12);
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Return(0));
        assert_eq!(&*env.written.borrow(), b"hello kernel");
    }

    #[test]
    fn log_rejects_out_of_range_pointer() {
        let env = env_with("hi");
        // Pointer past the mapped region.
        let r = req(Syscall::Log.number(), env.base + 100, 2);
        assert_eq!(
            dispatch(&r, &env),
            SyscallOutcome::Return(SyscallError::BadAddress.as_return())
        );
        assert!(env.written.borrow().is_empty());
    }

    #[test]
    fn log_rejects_oversize_len() {
        let env = env_with("x");
        let r = req(Syscall::Log.number(), env.base, (MAX_LOG_LEN + 1) as u64);
        assert_eq!(
            dispatch(&r, &env),
            SyscallOutcome::Return(SyscallError::InvalidArgument.as_return())
        );
    }

    #[test]
    fn unknown_syscall_is_rejected() {
        let env = env_with("");
        let r = req(9999, 0, 0);
        assert_eq!(
            dispatch(&r, &env),
            SyscallOutcome::Return(SyscallError::NoSuchCall.as_return())
        );
    }

    #[test]
    fn exit_reports_code() {
        let env = env_with("");
        let r = req(Syscall::Exit.number(), 7, 0);
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Exit(7));
    }

    #[test]
    fn yield_reports_yield() {
        let env = env_with("");
        let r = req(Syscall::Yield.number(), 0, 0);
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Yield);
    }

    #[test]
    fn now_returns_clock() {
        let env = env_with("");
        let r = req(Syscall::Now.number(), 0, 0);
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Return(123_456_789));
    }
}
