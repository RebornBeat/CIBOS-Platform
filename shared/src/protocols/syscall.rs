//! # Syscall ABI
//!
//! The contract between a CIBOS application (running in an isolated user
//! boundary) and the kernel. Like the boot contract in [`crate::protocols::boot`],
//! this is the single source of truth both sides bind to: the application's
//! runtime issues these numbers with this register convention, and the kernel's
//! trap dispatcher decodes them the same way.
//!
//! ## Why a narrow ABI
//!
//! The rich application surface (channels, lanes, timers, the network and
//! filesystem facades) is expressed in the SDK's `System` API. That API does not
//! need one syscall per method; it needs a small set of primitives the SDK
//! marshals onto. This module defines that primitive set. It starts deliberately
//! minimal — the operations needed to load an application, let it run, log, and
//! exit — and grows as the transport carries more of `System`. Keeping the raw
//! ABI small keeps the trusted trap path small.
//!
//! ## Register convention (x86_64)
//!
//! A user→kernel trap (`int 0x80` / `syscall`) passes:
//!
//! | role          | register |
//! |---------------|----------|
//! | syscall number| `rax`    |
//! | argument 0    | `rdi`    |
//! | argument 1    | `rsi`    |
//! | argument 2    | `rdx`    |
//! | return value  | `rax`    |
//!
//! Other architectures map the same six logical slots (number + up to three
//! args + return) onto their own ABI registers in the arch trap glue; this
//! module is architecture-neutral and only fixes the *logical* contract.
//!
//! Pointers passed in arguments are **user virtual addresses** in the calling
//! boundary's address space; the kernel validates and translates them before
//! use. A negative return value (as a two's-complement [`i64`]) is an error
//! code from [`SyscallError`]; zero or positive is success.

/// ABI version. Bumped on any incompatible change to numbers or convention.
pub const SYSCALL_ABI_VERSION: u32 = 1;

/// Syscall numbers. Stable identifiers placed in the number register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum Syscall {
    /// `log(ptr: *const u8, len: usize) -> 0`. Write `len` bytes of UTF-8 from
    /// the user buffer at `ptr` to the kernel console (serial + screen). The
    /// kernel bounds-checks and translates `ptr` within the caller's space.
    Log = 1,
    /// `exit(code: u64) -> !`. Terminate the calling application; control returns
    /// to the kernel, which tears down the boundary. Does not return to user.
    Exit = 2,
    /// `yield_now() -> 0`. Voluntarily yield the CPU back to the scheduler.
    Yield = 3,
    /// `now() -> u64`. Monotonic nanoseconds since boot, in the return register.
    Now = 4,
    /// `fs_read(args: *const FsRwArgs) -> isize`. Read the file named by the
    /// path in `args` into the user buffer in `args`; returns bytes read (>=0)
    /// or a negative [`SyscallError`]. See [`FsRwArgs`].
    FsRead = 5,
    /// `fs_write(args: *const FsRwArgs) -> isize`. Create/overwrite the file
    /// named by the path in `args` with the data buffer in `args`; returns bytes
    /// written (>=0) or a negative error.
    FsWrite = 6,
    /// `fs_mkdir(path_ptr: *const u8, path_len: usize) -> 0`. Create a directory.
    FsMkdir = 7,
    /// `fs_exists(path_ptr: *const u8, path_len: usize) -> 0|1`. Whether a path
    /// exists.
    FsExists = 8,
    /// `read_key(blocking: u64) -> i64`. Read the next keyboard event. With
    /// `blocking == 0` returns immediately ([`SyscallError::NotFound`] if the
    /// queue is empty); with `blocking != 0` waits (via the system timer) until a
    /// key is available. On success the event is packed into the return value by
    /// [`encode_key`]; decode it with [`decode_key`].
    ReadKey = 9,
    /// `get_random(buf_ptr: *mut u8, len: usize) -> isize`. Fill `len` bytes at
    /// `buf_ptr` with cryptographically-random bytes from the kernel CSPRNG;
    /// returns the number of bytes written or a negative error.
    GetRandom = 10,
    /// `fs_list(args: *const FsRwArgs) -> isize`. List the directory named by the
    /// path in `args` into the user buffer in `args`, as the entry names joined
    /// by `\n` (no trailing newline); returns the number of bytes written
    /// (>=0, truncated to `buf_len`) or a negative [`SyscallError`]. Reuses the
    /// [`FsRwArgs`] block (path = directory, buf = destination).
    FsList = 11,
    /// `fs_delete(path_ptr: *const u8, path_len: usize) -> 0`. Remove the file
    /// named by the path; returns 0 or a negative error
    /// ([`SyscallError::NotFound`] if absent).
    FsDelete = 12,
}

impl Syscall {
    /// Decode a raw syscall number, or `None` if unknown.
    #[must_use]
    pub const fn from_number(n: u64) -> Option<Self> {
        match n {
            1 => Some(Syscall::Log),
            2 => Some(Syscall::Exit),
            3 => Some(Syscall::Yield),
            4 => Some(Syscall::Now),
            5 => Some(Syscall::FsRead),
            6 => Some(Syscall::FsWrite),
            7 => Some(Syscall::FsMkdir),
            8 => Some(Syscall::FsExists),
            9 => Some(Syscall::ReadKey),
            10 => Some(Syscall::GetRandom),
            11 => Some(Syscall::FsList),
            12 => Some(Syscall::FsDelete),
            _ => None,
        }
    }

    /// The raw number for this syscall.
    #[must_use]
    pub const fn number(self) -> u64 {
        self as u64
    }
}

/// Packed argument block for [`Syscall::FsRead`] / [`Syscall::FsWrite`], passed
/// by pointer (the three-register ABI cannot carry four pointers/lengths
/// directly). All four fields are user virtual addresses / lengths in the
/// calling boundary. Encoded little-endian as four `u64`s = [`FS_RW_ARGS_LEN`]
/// bytes.
///
/// * `path_ptr` / `path_len` — the file path (e.g. `b"/etc/passwd"`).
/// * `buf_ptr` / `buf_len` — for `fs_write`, the data to write; for `fs_read`,
///   the destination buffer (the call reads at most `buf_len` bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsRwArgs {
    /// User pointer to the path bytes.
    pub path_ptr: u64,
    /// Length of the path in bytes.
    pub path_len: u64,
    /// User pointer to the data/destination buffer.
    pub buf_ptr: u64,
    /// Length of the data/destination buffer in bytes.
    pub buf_len: u64,
}

/// Encoded size of [`FsRwArgs`] in bytes.
pub const FS_RW_ARGS_LEN: usize = 32;

/// A decoded keyboard event from [`Syscall::ReadKey`]. Printable keys carry a
/// `char`; the rest are named. This mirrors the kernel's input model but lives
/// in the ABI crate so the kernel and applications share one wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    /// A printable character (case already resolved).
    Char(char),
    /// Return/Enter.
    Enter,
    /// Backspace.
    Backspace,
    /// Forward delete.
    Delete,
    /// Tab.
    Tab,
    /// Escape.
    Escape,
    /// Arrow / navigation keys.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Home.
    Home,
    /// End.
    End,
}

/// Modifier state accompanying a [`KeyCode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyMods {
    /// Shift held.
    pub shift: bool,
    /// Control held.
    pub ctrl: bool,
    /// Alt held.
    pub alt: bool,
}

const KEY_KIND_SHIFT: u64 = 32;
const KEY_MODS_SHIFT: u64 = 40;
const KEY_CHAR_MASK: u64 = 0xFFFF_FFFF;

fn key_kind(code: KeyCode) -> u64 {
    match code {
        KeyCode::Char(_) => 0,
        KeyCode::Enter => 1,
        KeyCode::Backspace => 2,
        KeyCode::Delete => 3,
        KeyCode::Tab => 4,
        KeyCode::Escape => 5,
        KeyCode::Left => 6,
        KeyCode::Right => 7,
        KeyCode::Up => 8,
        KeyCode::Down => 9,
        KeyCode::Home => 10,
        KeyCode::End => 11,
    }
}

/// Pack a key event into the non-negative `i64` returned by [`Syscall::ReadKey`].
/// Layout: bits [31:0] = char scalar (for `Char`), [39:32] = kind tag,
/// [42:40] = shift/ctrl/alt bits.
#[must_use]
pub fn encode_key(code: KeyCode, mods: KeyMods) -> i64 {
    let ch = if let KeyCode::Char(c) = code {
        c as u64
    } else {
        0
    };
    let m = (mods.shift as u64) | ((mods.ctrl as u64) << 1) | ((mods.alt as u64) << 2);
    let raw = (ch & KEY_CHAR_MASK) | (key_kind(code) << KEY_KIND_SHIFT) | (m << KEY_MODS_SHIFT);
    // Always fits in 43 bits, so it is a non-negative i64.
    raw as i64
}

/// Decode a value packed by [`encode_key`]. Returns `None` for the (negative)
/// no-key / error returns.
#[must_use]
pub fn decode_key(ret: i64) -> Option<(KeyCode, KeyMods)> {
    if ret < 0 {
        return None;
    }
    let raw = ret as u64;
    let kind = (raw >> KEY_KIND_SHIFT) & 0xFF;
    let m = (raw >> KEY_MODS_SHIFT) & 0x7;
    let mods = KeyMods {
        shift: m & 1 != 0,
        ctrl: m & 2 != 0,
        alt: m & 4 != 0,
    };
    let code = match kind {
        0 => KeyCode::Char(char::from_u32((raw & KEY_CHAR_MASK) as u32)?),
        1 => KeyCode::Enter,
        2 => KeyCode::Backspace,
        3 => KeyCode::Delete,
        4 => KeyCode::Tab,
        5 => KeyCode::Escape,
        6 => KeyCode::Left,
        7 => KeyCode::Right,
        8 => KeyCode::Up,
        9 => KeyCode::Down,
        10 => KeyCode::Home,
        11 => KeyCode::End,
        _ => return None,
    };
    Some((code, mods))
}

impl FsRwArgs {
    /// Encode to the 32-byte little-endian layout.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; FS_RW_ARGS_LEN] {
        let mut b = [0u8; FS_RW_ARGS_LEN];
        b[0..8].copy_from_slice(&self.path_ptr.to_le_bytes());
        b[8..16].copy_from_slice(&self.path_len.to_le_bytes());
        b[16..24].copy_from_slice(&self.buf_ptr.to_le_bytes());
        b[24..32].copy_from_slice(&self.buf_len.to_le_bytes());
        b
    }

    /// Decode from the 32-byte little-endian layout.
    #[must_use]
    pub fn from_bytes(b: &[u8; FS_RW_ARGS_LEN]) -> Self {
        let u = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        FsRwArgs {
            path_ptr: u(0),
            path_len: u(8),
            buf_ptr: u(16),
            buf_len: u(24),
        }
    }
}

/// Error codes returned (as negated [`i64`]) in the return register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum SyscallError {
    /// Unknown syscall number.
    NoSuchCall = 1,
    /// A pointer argument was outside the caller's mapped memory.
    BadAddress = 2,
    /// An argument was invalid (e.g. a length that overflows the buffer).
    InvalidArgument = 3,
    /// The operation is not permitted for this boundary.
    NotPermitted = 4,
    /// The named path does not exist.
    NotFound = 5,
    /// A storage/filesystem operation failed (I/O error, corrupt, no space,
    /// wrong kind, too large).
    IoError = 6,
}

impl SyscallError {
    /// Encode as the negative return value the ABI uses for errors.
    #[must_use]
    pub const fn as_return(self) -> i64 {
        -(self as i64)
    }

    /// Decode an error from a (negative) return value, or `None` if it is not a
    /// recognized error or is a success value (>= 0).
    #[must_use]
    pub const fn from_return(v: i64) -> Option<Self> {
        match v {
            -1 => Some(SyscallError::NoSuchCall),
            -2 => Some(SyscallError::BadAddress),
            -3 => Some(SyscallError::InvalidArgument),
            -4 => Some(SyscallError::NotPermitted),
            -5 => Some(SyscallError::NotFound),
            -6 => Some(SyscallError::IoError),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syscall_numbers_roundtrip() {
        for s in [
            Syscall::Log,
            Syscall::Exit,
            Syscall::Yield,
            Syscall::Now,
            Syscall::FsRead,
            Syscall::FsWrite,
            Syscall::FsMkdir,
            Syscall::FsExists,
            Syscall::ReadKey,
            Syscall::GetRandom,
            Syscall::FsList,
            Syscall::FsDelete,
        ] {
            assert_eq!(Syscall::from_number(s.number()), Some(s));
        }
        assert_eq!(Syscall::from_number(0), None);
        assert_eq!(Syscall::from_number(999), None);
    }

    #[test]
    fn key_encode_decode_roundtrip() {
        let cases = [
            (KeyCode::Char('a'), KeyMods::default()),
            (KeyCode::Char('Z'), KeyMods { shift: true, ctrl: false, alt: false }),
            (KeyCode::Char('c'), KeyMods { shift: false, ctrl: true, alt: false }),
            (KeyCode::Enter, KeyMods::default()),
            (KeyCode::Backspace, KeyMods::default()),
            (KeyCode::Left, KeyMods { shift: false, ctrl: false, alt: true }),
            (KeyCode::Escape, KeyMods::default()),
        ];
        for (code, mods) in cases {
            let enc = encode_key(code, mods);
            assert!(enc >= 0, "encoded key must be non-negative");
            assert_eq!(decode_key(enc), Some((code, mods)));
        }
        // Negative returns decode to None (no key / error).
        assert_eq!(decode_key(-1), None);
        assert_eq!(decode_key(SyscallError::NotFound.as_return()), None);
    }

    #[test]
    fn fs_rw_args_roundtrip() {
        let a = FsRwArgs {
            path_ptr: 0x1111,
            path_len: 7,
            buf_ptr: 0x2222_3333,
            buf_len: 4096,
        };
        assert_eq!(FsRwArgs::from_bytes(&a.to_bytes()), a);
    }

    #[test]
    fn error_codes_roundtrip() {
        for e in [
            SyscallError::NoSuchCall,
            SyscallError::BadAddress,
            SyscallError::InvalidArgument,
            SyscallError::NotPermitted,
            SyscallError::NotFound,
            SyscallError::IoError,
        ] {
            assert!(e.as_return() < 0);
            assert_eq!(SyscallError::from_return(e.as_return()), Some(e));
        }
        // Success values are not errors.
        assert_eq!(SyscallError::from_return(0), None);
        assert_eq!(SyscallError::from_return(42), None);
    }
}
