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

use shared::protocols::syscall::{FsRwArgs, Syscall, SyscallError, FS_RW_ARGS_LEN};
use shared::BoundaryId;

/// Maximum bytes a single `log` may emit, to bound kernel work per call.
pub const MAX_LOG_LEN: usize = 4096;

/// Maximum path length the filesystem syscalls accept, to bound kernel work.
pub const MAX_PATH_LEN: usize = 1024;

/// Maximum bytes a single `fs_read`/`fs_write` may transfer, to bound kernel
/// buffering per call.
pub const MAX_FS_IO_LEN: usize = 64 * 1024;

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

    /// Copy `bytes` into the user buffer at virtual address `ptr` (in
    /// `boundary`'s space). Returns `Err(BadAddress)` if the range is not fully
    /// mapped and writable in that boundary.
    fn copy_to_user(
        &self,
        boundary: BoundaryId,
        ptr: u64,
        bytes: &[u8],
    ) -> Result<(), SyscallError>;

    /// Read the whole file at `path` into a kernel buffer. `Ok(None)` if the
    /// path does not exist; `Err` for other failures. The default rejects all
    /// access (a kernel without a mounted filesystem); the real environment
    /// overrides these four.
    fn fs_read(&self, _path: &[u8]) -> Result<Option<alloc::vec::Vec<u8>>, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Create/overwrite the file at `path` with `data`. `Ok(())` on success.
    fn fs_write(&self, _path: &[u8], _data: &[u8]) -> Result<(), SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Create a directory at `path`.
    fn fs_mkdir(&self, _path: &[u8]) -> Result<(), SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Whether `path` exists.
    fn fs_exists(&self, _path: &[u8]) -> Result<bool, SyscallError> {
        Err(SyscallError::NotPermitted)
    }

    /// Read the next keyboard event. `blocking` selects waiting vs immediate.
    /// Returns the packed key value (>= 0, via `encode_key`) or a negative
    /// [`SyscallError`] (`NotFound` when non-blocking and the queue is empty).
    /// The default reports no input device.
    fn read_key(&self, _blocking: bool) -> i64 {
        SyscallError::NotFound.as_return()
    }

    /// Fill `out` with cryptographically-random bytes from the kernel CSPRNG.
    /// The default reports no entropy source.
    fn fill_random(&self, _out: &mut [u8]) -> Result<(), SyscallError> {
        Err(SyscallError::NotPermitted)
    }
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
        Syscall::FsRead => fs_read(req, env),
        Syscall::FsWrite => fs_write(req, env),
        Syscall::FsMkdir => {
            match read_path(req.boundary, req.arg0, req.arg1 as usize, env) {
                Ok(path) => match env.fs_mkdir(&path) {
                    Ok(()) => SyscallOutcome::Return(0),
                    Err(e) => SyscallOutcome::Return(e.as_return()),
                },
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::FsExists => {
            match read_path(req.boundary, req.arg0, req.arg1 as usize, env) {
                Ok(path) => match env.fs_exists(&path) {
                    Ok(true) => SyscallOutcome::Return(1),
                    Ok(false) => SyscallOutcome::Return(0),
                    Err(e) => SyscallOutcome::Return(e.as_return()),
                },
                Err(e) => SyscallOutcome::Return(e.as_return()),
            }
        }
        Syscall::ReadKey => SyscallOutcome::Return(env.read_key(req.arg0 != 0)),
        Syscall::GetRandom => get_random(req, env),
    }
}

/// `get_random`: fill the user buffer at `arg0` (len `arg1`) with CSPRNG bytes.
fn get_random<E: SyscallEnv>(req: &SyscallRequest, env: &E) -> SyscallOutcome {
    let len = req.arg1 as usize;
    if len == 0 || len > MAX_FS_IO_LEN {
        return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
    }
    let mut buf = alloc::vec![0u8; len];
    if let Err(e) = env.fill_random(&mut buf) {
        return SyscallOutcome::Return(e.as_return());
    }
    if let Err(e) = env.copy_to_user(req.boundary, req.arg0, &buf) {
        return SyscallOutcome::Return(e.as_return());
    }
    SyscallOutcome::Return(len as i64)
}

/// Copy a bounded path from user memory into a kernel `Vec`.
fn read_path<E: SyscallEnv>(
    boundary: BoundaryId,
    ptr: u64,
    len: usize,
    env: &E,
) -> Result<alloc::vec::Vec<u8>, SyscallError> {
    if len == 0 || len > MAX_PATH_LEN {
        return Err(SyscallError::InvalidArgument);
    }
    let mut buf = alloc::vec![0u8; len];
    env.copy_from_user(boundary, ptr, len, &mut buf)?;
    Ok(buf)
}

/// Decode the `FsRwArgs` block from user memory at `ptr`.
fn read_rw_args<E: SyscallEnv>(
    boundary: BoundaryId,
    ptr: u64,
    env: &E,
) -> Result<FsRwArgs, SyscallError> {
    let mut raw = [0u8; FS_RW_ARGS_LEN];
    env.copy_from_user(boundary, ptr, FS_RW_ARGS_LEN, &mut raw)?;
    Ok(FsRwArgs::from_bytes(&raw))
}

/// `fs_read`: read the file named by the path in the arg block into the user
/// buffer in the arg block; return bytes read or a negative error.
fn fs_read<E: SyscallEnv>(req: &SyscallRequest, env: &E) -> SyscallOutcome {
    let args = match read_rw_args(req.boundary, req.arg0, env) {
        Ok(a) => a,
        Err(e) => return SyscallOutcome::Return(e.as_return()),
    };
    if args.buf_len as usize > MAX_FS_IO_LEN {
        return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
    }
    let path = match read_path(req.boundary, args.path_ptr, args.path_len as usize, env) {
        Ok(p) => p,
        Err(e) => return SyscallOutcome::Return(e.as_return()),
    };
    let data = match env.fs_read(&path) {
        Ok(Some(d)) => d,
        Ok(None) => return SyscallOutcome::Return(SyscallError::NotFound.as_return()),
        Err(e) => return SyscallOutcome::Return(e.as_return()),
    };
    // Copy up to buf_len bytes back to the user; return the count.
    let n = core::cmp::min(data.len(), args.buf_len as usize);
    if let Err(e) = env.copy_to_user(req.boundary, args.buf_ptr, &data[..n]) {
        return SyscallOutcome::Return(e.as_return());
    }
    SyscallOutcome::Return(n as i64)
}

/// `fs_write`: create/overwrite the file named by the path in the arg block with
/// the data buffer in the arg block; return bytes written or a negative error.
fn fs_write<E: SyscallEnv>(req: &SyscallRequest, env: &E) -> SyscallOutcome {
    let args = match read_rw_args(req.boundary, req.arg0, env) {
        Ok(a) => a,
        Err(e) => return SyscallOutcome::Return(e.as_return()),
    };
    let dlen = args.buf_len as usize;
    if dlen > MAX_FS_IO_LEN {
        return SyscallOutcome::Return(SyscallError::InvalidArgument.as_return());
    }
    let path = match read_path(req.boundary, args.path_ptr, args.path_len as usize, env) {
        Ok(p) => p,
        Err(e) => return SyscallOutcome::Return(e.as_return()),
    };
    let mut data = alloc::vec![0u8; dlen];
    if dlen > 0 {
        if let Err(e) = env.copy_from_user(req.boundary, args.buf_ptr, dlen, &mut data) {
            return SyscallOutcome::Return(e.as_return());
        }
    }
    match env.fs_write(&path, &data) {
        Ok(()) => SyscallOutcome::Return(dlen as i64),
        Err(e) => SyscallOutcome::Return(e.as_return()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;

    struct MockEnv {
        // A single mapped region [base, base+data.len()) in the test boundary.
        base: u64,
        data: RefCell<alloc::vec::Vec<u8>>,
        written: RefCell<alloc::vec::Vec<u8>>,
        clock: u64,
        // A tiny in-memory filesystem: path -> contents; dirs tracked as a set.
        files: RefCell<alloc::collections::BTreeMap<alloc::vec::Vec<u8>, alloc::vec::Vec<u8>>>,
        dirs: RefCell<alloc::collections::BTreeSet<alloc::vec::Vec<u8>>>,
    }

    impl SyscallEnv for MockEnv {
        fn copy_from_user(
            &self,
            _boundary: BoundaryId,
            ptr: u64,
            len: usize,
            out: &mut [u8],
        ) -> Result<(), SyscallError> {
            let data = self.data.borrow();
            let start = ptr
                .checked_sub(self.base)
                .ok_or(SyscallError::BadAddress)? as usize;
            let end = start.checked_add(len).ok_or(SyscallError::InvalidArgument)?;
            if end > data.len() {
                return Err(SyscallError::BadAddress);
            }
            out[..len].copy_from_slice(&data[start..end]);
            Ok(())
        }
        fn console_write(&self, bytes: &[u8]) {
            self.written.borrow_mut().extend_from_slice(bytes);
        }
        fn now_nanos(&self) -> u64 {
            self.clock
        }
        fn copy_to_user(
            &self,
            _boundary: BoundaryId,
            ptr: u64,
            bytes: &[u8],
        ) -> Result<(), SyscallError> {
            let mut data = self.data.borrow_mut();
            let start = ptr
                .checked_sub(self.base)
                .ok_or(SyscallError::BadAddress)? as usize;
            let end = start.checked_add(bytes.len()).ok_or(SyscallError::InvalidArgument)?;
            if end > data.len() {
                return Err(SyscallError::BadAddress);
            }
            data[start..end].copy_from_slice(bytes);
            Ok(())
        }
        fn fs_read(&self, path: &[u8]) -> Result<Option<alloc::vec::Vec<u8>>, SyscallError> {
            Ok(self.files.borrow().get(path).cloned())
        }
        fn fs_write(&self, path: &[u8], data: &[u8]) -> Result<(), SyscallError> {
            self.files.borrow_mut().insert(path.to_vec(), data.to_vec());
            Ok(())
        }
        fn fs_mkdir(&self, path: &[u8]) -> Result<(), SyscallError> {
            self.dirs.borrow_mut().insert(path.to_vec());
            Ok(())
        }
        fn fs_exists(&self, path: &[u8]) -> Result<bool, SyscallError> {
            Ok(self.files.borrow().contains_key(path) || self.dirs.borrow().contains(path))
        }
        fn read_key(&self, _blocking: bool) -> i64 {
            use shared::protocols::syscall::{encode_key, KeyCode, KeyMods};
            // The mock yields a single 'k' keypress.
            encode_key(KeyCode::Char('k'), KeyMods::default())
        }
        fn fill_random(&self, out: &mut [u8]) -> Result<(), SyscallError> {
            // Deterministic non-zero fill for the test.
            for (i, b) in out.iter_mut().enumerate() {
                *b = (i as u8).wrapping_mul(31).wrapping_add(7);
            }
            Ok(())
        }
    }

    fn env_with(text: &str) -> MockEnv {
        MockEnv {
            base: 0x4000_0000,
            data: RefCell::new(text.as_bytes().to_vec()),
            written: RefCell::new(alloc::vec::Vec::new()),
            clock: 123_456_789,
            files: RefCell::new(alloc::collections::BTreeMap::new()),
            dirs: RefCell::new(alloc::collections::BTreeSet::new()),
        }
    }

    /// Build an env whose user memory is exactly `bytes` at `base`.
    fn env_bytes(bytes: &[u8]) -> MockEnv {
        let e = env_with("");
        *e.data.borrow_mut() = bytes.to_vec();
        e
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

    #[test]
    fn read_key_returns_encoded_event() {
        use shared::protocols::syscall::{decode_key, KeyCode, KeyMods};
        let env = env_with("");
        let r = req(Syscall::ReadKey.number(), 1, 0);
        let out = dispatch(&r, &env);
        let SyscallOutcome::Return(v) = out else {
            panic!("expected Return");
        };
        assert_eq!(decode_key(v), Some((KeyCode::Char('k'), KeyMods::default())));
    }

    #[test]
    fn get_random_fills_user_buffer() {
        // User memory is a 16-byte zeroed region at base; GetRandom must fill it.
        let env = env_bytes(&[0u8; 16]);
        let r = SyscallRequest {
            number: Syscall::GetRandom.number(),
            arg0: 0x4000_0000,
            arg1: 16,
            arg2: 0,
            boundary: BoundaryId::new(1),
        };
        assert_eq!(dispatch(&r, &env), SyscallOutcome::Return(16));
        // The buffer is no longer all-zero (the mock filled it deterministically).
        assert!(env.data.borrow().iter().any(|&b| b != 0));
        // Zero length is rejected.
        let r0 = req(Syscall::GetRandom.number(), 0x4000_0000, 0);
        assert_eq!(
            dispatch(&r0, &env),
            SyscallOutcome::Return(SyscallError::InvalidArgument.as_return())
        );
    }

    #[test]
    fn fs_write_then_read_through_syscalls() {
        // User memory layout: [path bytes][data bytes][FsRwArgs block][read buf].
        let path = b"/etc/passwd";
        let data = b"alice:hash";
        let base = 0x4000_0000u64;
        let path_off = 0usize;
        let data_off = path.len();
        let args_off = data_off + data.len();
        let readbuf_off = args_off + FS_RW_ARGS_LEN;
        let readbuf_len = 32usize;

        let write_args = FsRwArgs {
            path_ptr: base + path_off as u64,
            path_len: path.len() as u64,
            buf_ptr: base + data_off as u64,
            buf_len: data.len() as u64,
        };

        let mut mem = alloc::vec::Vec::new();
        mem.extend_from_slice(path);
        mem.extend_from_slice(data);
        mem.extend_from_slice(&write_args.to_bytes());
        mem.extend_from_slice(&[0u8; 32]); // read buffer space
        let env = env_bytes(&mem);

        // fs_write(args_ptr)
        let wr = SyscallRequest {
            number: Syscall::FsWrite.number(),
            arg0: base + args_off as u64,
            arg1: 0,
            arg2: 0,
            boundary: BoundaryId::new(1),
        };
        assert_eq!(dispatch(&wr, &env), SyscallOutcome::Return(data.len() as i64));
        assert_eq!(env.files.borrow().get(&path.to_vec()).unwrap(), data);

        // fs_read(args_ptr) into the read buffer; reuse an args block pointing at
        // the read buffer.
        let read_args = FsRwArgs {
            path_ptr: base + path_off as u64,
            path_len: path.len() as u64,
            buf_ptr: base + readbuf_off as u64,
            buf_len: readbuf_len as u64,
        };
        // Overwrite the args block in user memory with the read args.
        env.data.borrow_mut()[args_off..args_off + FS_RW_ARGS_LEN]
            .copy_from_slice(&read_args.to_bytes());
        let rr = SyscallRequest {
            number: Syscall::FsRead.number(),
            arg0: base + args_off as u64,
            arg1: 0,
            arg2: 0,
            boundary: BoundaryId::new(1),
        };
        assert_eq!(dispatch(&rr, &env), SyscallOutcome::Return(data.len() as i64));
        // The data landed in the read buffer.
        let got = &env.data.borrow()[readbuf_off..readbuf_off + data.len()];
        assert_eq!(got, data);
    }

    #[test]
    fn fs_read_missing_is_not_found() {
        // Layout: [path at 0][args block at 64][read buf at 96].
        let path = b"/nope";
        let base = 0x4000_0000u64;
        let args = FsRwArgs {
            path_ptr: base,
            path_len: path.len() as u64,
            buf_ptr: base + 96,
            buf_len: 16,
        };
        let mut mem = alloc::vec::Vec::new();
        mem.extend_from_slice(path);
        mem.resize(64, 0);
        mem.extend_from_slice(&args.to_bytes()); // 64..96
        mem.resize(96 + 16, 0); // read buffer
        let env = env_bytes(&mem);
        let r = SyscallRequest {
            number: Syscall::FsRead.number(),
            arg0: base + 64,
            arg1: 0,
            arg2: 0,
            boundary: BoundaryId::new(1),
        };
        assert_eq!(
            dispatch(&r, &env),
            SyscallOutcome::Return(SyscallError::NotFound.as_return())
        );
    }

    #[test]
    fn fs_mkdir_and_exists() {
        let path = b"/home";
        let base = 0x4000_0000u64;
        let env = env_bytes(path);
        let mk = req(Syscall::FsMkdir.number(), base, path.len() as u64);
        assert_eq!(dispatch(&mk, &env), SyscallOutcome::Return(0));
        let ex = req(Syscall::FsExists.number(), base, path.len() as u64);
        assert_eq!(dispatch(&ex, &env), SyscallOutcome::Return(1));
        // A different path does not exist.
        let env2 = env_bytes(b"/other");
        let ex2 = req(Syscall::FsExists.number(), base, 6);
        assert_eq!(dispatch(&ex2, &env2), SyscallOutcome::Return(0));
    }

    #[test]
    fn fs_default_env_denies() {
        // The default trait methods reject access (kernel with no filesystem).
        struct Bare {
            base: u64,
            data: alloc::vec::Vec<u8>,
        }
        impl SyscallEnv for Bare {
            fn copy_from_user(&self, _b: BoundaryId, ptr: u64, len: usize, out: &mut [u8]) -> Result<(), SyscallError> {
                let s = ptr.checked_sub(self.base).ok_or(SyscallError::BadAddress)? as usize;
                let e = s.checked_add(len).ok_or(SyscallError::InvalidArgument)?;
                if e > self.data.len() { return Err(SyscallError::BadAddress); }
                out[..len].copy_from_slice(&self.data[s..e]);
                Ok(())
            }
            fn console_write(&self, _b: &[u8]) {}
            fn now_nanos(&self) -> u64 { 0 }
            fn copy_to_user(&self, _b: BoundaryId, _p: u64, _by: &[u8]) -> Result<(), SyscallError> {
                Ok(())
            }
        }
        let env = Bare { base: 0x4000_0000, data: b"/x".to_vec() };
        let r = req(Syscall::FsExists.number(), 0x4000_0000, 2);
        assert_eq!(
            dispatch(&r, &env),
            SyscallOutcome::Return(SyscallError::NotPermitted.as_return())
        );
    }
}
