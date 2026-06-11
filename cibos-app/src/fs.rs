//! Filesystem access for a CIBOS application, over the `Fs*` syscalls.
//!
//! [`read_into`] fills a caller-provided buffer (alloc-free); [`write`] stores a
//! whole file; [`mkdir`] and [`exists`] manage and test paths. Paths and data
//! are passed as byte slices; the kernel resolves them against its mounted root
//! filesystem (CIBOSFS) and validates every pointer against this boundary.

use crate::syscall::{decode, syscall3};
use shared::protocols::syscall::{FsRwArgs, Syscall, SyscallError};

/// Read the file at `path` into `buf`, returning the number of bytes read
/// (which may be less than the file size if `buf` is smaller). The file
/// contents are truncated to `buf.len()`.
///
/// # Errors
///
/// [`SyscallError::NotFound`] if the path does not exist, or another error from
/// the kernel.
pub fn read_into(path: &[u8], buf: &mut [u8]) -> Result<usize, SyscallError> {
    let args = FsRwArgs {
        path_ptr: path.as_ptr() as u64,
        path_len: path.len() as u64,
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_len: buf.len() as u64,
    };
    let raw = args.to_bytes();
    // SAFETY: `raw` is a valid readable 32-byte block; path/buf pointers and
    // lengths describe valid slices the kernel validates against this boundary.
    let ret = unsafe { syscall3(Syscall::FsRead, raw.as_ptr() as u64, 0, 0) };
    Ok(decode(ret)? as usize)
}

/// Create or overwrite the file at `path` with `data`, returning the number of
/// bytes written.
///
/// # Errors
///
/// A kernel error (e.g. [`SyscallError::IoError`] if the volume is full or the
/// file is too large).
pub fn write(path: &[u8], data: &[u8]) -> Result<usize, SyscallError> {
    let args = FsRwArgs {
        path_ptr: path.as_ptr() as u64,
        path_len: path.len() as u64,
        buf_ptr: data.as_ptr() as u64,
        buf_len: data.len() as u64,
    };
    let raw = args.to_bytes();
    // SAFETY: as `read_into`.
    let ret = unsafe { syscall3(Syscall::FsWrite, raw.as_ptr() as u64, 0, 0) };
    Ok(decode(ret)? as usize)
}

/// Create a directory at `path`.
///
/// # Errors
///
/// A kernel error (e.g. [`SyscallError::IoError`] if it already exists or the
/// parent is missing).
pub fn mkdir(path: &[u8]) -> Result<(), SyscallError> {
    // SAFETY: `path` is a valid slice; the kernel validates it.
    let ret = unsafe { syscall3(Syscall::FsMkdir, path.as_ptr() as u64, path.len() as u64, 0) };
    decode(ret).map(|_| ())
}

/// Whether `path` exists.
///
/// # Errors
///
/// A kernel error if the filesystem is unavailable.
pub fn exists(path: &[u8]) -> Result<bool, SyscallError> {
    // SAFETY: `path` is a valid slice; the kernel validates it.
    let ret = unsafe { syscall3(Syscall::FsExists, path.as_ptr() as u64, path.len() as u64, 0) };
    Ok(decode(ret)? != 0)
}
