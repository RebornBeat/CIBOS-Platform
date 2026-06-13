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

/// Read the whole file at `path` into an owned `Vec`, or `None` if it does not
/// exist (or cannot be read). Uses a bounded heap buffer (64 KiB), so very large
/// files are truncated to the cap; for the package repo and shell working files
/// this is ample. The buffer is on the heap, since the app stack is one page.
#[must_use]
pub fn read_into_vec(path: &[u8]) -> Option<alloc::vec::Vec<u8>> {
    let mut buf = alloc::vec![0u8; 64 * 1024];
    match read_into(path, &mut buf) {
        Ok(n) => {
            buf.truncate(n);
            Some(buf)
        }
        Err(_) => None,
    }
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

/// List the directory at `path` into `buf`, returning the number of bytes
/// written. The listing is the entry names joined by `\n` (no trailing
/// newline), truncated to `buf.len()`.
///
/// # Errors
///
/// [`SyscallError::NotFound`] if the path is not a directory, or another kernel
/// error.
pub fn list_into(path: &[u8], buf: &mut [u8]) -> Result<usize, SyscallError> {
    let args = FsRwArgs {
        path_ptr: path.as_ptr() as u64,
        path_len: path.len() as u64,
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_len: buf.len() as u64,
    };
    let raw = args.to_bytes();
    // SAFETY: as `read_into`.
    let ret = unsafe { syscall3(Syscall::FsList, raw.as_ptr() as u64, 0, 0) };
    Ok(decode(ret)? as usize)
}

/// List the directory at `path`, returning the entry names as owned strings.
/// Uses the heap; the listing is read into a bounded scratch buffer.
///
/// # Errors
///
/// As [`list_into`].
pub fn list(path: &[u8]) -> Result<alloc::vec::Vec<alloc::string::String>, SyscallError> {
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    // A page of listing is plenty for a directory of names; truncation only
    // drops trailing entries, never corrupts (names are whole up to the cut).
    // The buffer lives on the heap, not the stack: the per-app stack is a single
    // page, so a 4 KiB stack array would overflow it. The app heap (mapped by
    // the kernel) is the right place for transient buffers like this.
    let mut buf = alloc::vec![0u8; 4096];
    let n = list_into(path, &mut buf)?;
    let text = core::str::from_utf8(&buf[..n]).unwrap_or("");
    let names: Vec<String> = text
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();
    Ok(names)
}

/// Delete the file at `path`.
///
/// # Errors
///
/// [`SyscallError::NotFound`] if the path does not exist, or another kernel
/// error (e.g. it names a directory).
pub fn delete(path: &[u8]) -> Result<(), SyscallError> {
    // SAFETY: `path` is a valid slice; the kernel validates it.
    let ret = unsafe { syscall3(Syscall::FsDelete, path.as_ptr() as u64, path.len() as u64, 0) };
    decode(ret).map(|_| ())
}
