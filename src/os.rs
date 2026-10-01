//! What qj needs from the operating system beyond `std`, on Unix and on
//! Windows: byte views of OS strings, I/O on the C library's file descriptors
//! with its `errno`, and the home directory.
//!
//! jq does its I/O through the C library, and on Windows that shows. The C
//! runtime (UCRT, which jq's Windows release binary links) opens standard
//! input, output and error, and the files jq reads, in text mode: `\n` is
//! written as `\r\n`, `\r\n` is read as `\n`, and Ctrl-Z ends the input. jq's
//! `-b` puts the three standard streams in binary mode; input files stay in
//! text mode. qj reads and writes through the same descriptors so that it
//! does the same, and reports failures with the C runtime's `errno`, which is
//! what jq's messages print (on Windows, `io::Error::last_os_error` is
//! `GetLastError` instead).
//!
//! When standard output is a console, jq writes to it with `WriteConsoleW`,
//! as UTF-16, rather than through the C runtime, and enables color only if
//! `ANSICON` is set or the console takes virtual-terminal sequences
//! ([`Console`]).

use std::ffi::{OsStr, OsString};
use std::io::{self, Read};

#[cfg(unix)]
pub use std::os::unix::ffi::{OsStrExt, OsStringExt};

#[cfg(windows)]
pub use self::windows::{Console, CrtFile, OsStrExt, OsStringExt, binary_stdio, init};

/// `read(fd, buf, len)`, with `errno` as the error.
pub fn read_fd(fd: i32, buf: &mut [u8]) -> io::Result<usize> {
    #[cfg(unix)]
    // SAFETY: `buf` is valid for writes of its length.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    #[cfg(windows)]
    // SAFETY: as above, for at most `len` bytes, which fits the C runtime's
    // `unsigned int` count.
    let n = unsafe {
        let len = buf.len().min(1 << 30) as libc::c_uint;
        libc::read(fd, buf.as_mut_ptr().cast(), len) as isize
    };
    if n < 0 {
        Err(errno_error())
    } else {
        Ok(n as usize)
    }
}

/// `write(fd, buf, len)`, with `errno` as the error.
pub fn write_fd(fd: i32, buf: &[u8]) -> io::Result<usize> {
    #[cfg(unix)]
    // SAFETY: `buf` is valid for reads of its length.
    let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
    #[cfg(windows)]
    // SAFETY: as above, for at most `len` bytes.
    let n = unsafe {
        let len = buf.len().min(1 << 30) as libc::c_uint;
        libc::write(fd, buf.as_ptr().cast(), len) as isize
    };
    if n < 0 {
        Err(errno_error())
    } else {
        Ok(n as usize)
    }
}

/// The C library's `errno` as an [`io::Error`] whose `raw_os_error` is that
/// `errno`, for `strerror` (`crate::jq::platform::strerror`).
pub fn errno_error() -> io::Error {
    #[cfg(unix)]
    {
        io::Error::last_os_error()
    }
    #[cfg(windows)]
    {
        // SAFETY: `_errno` returns this thread's `errno`.
        io::Error::from_raw_os_error(unsafe { *windows::_errno() })
    }
}

/// Writes `bytes` to standard error, as jq's unbuffered `stderr` does, giving
/// up on errors (there is nowhere to report them).
pub fn write_stderr(bytes: &[u8]) {
    #[cfg(unix)]
    {
        use std::io::Write;
        let _ = io::stderr().write_all(bytes);
    }
    #[cfg(windows)]
    {
        // Through the C runtime, whose text mode jq's stderr has.
        let mut rest = bytes;
        while !rest.is_empty() {
            match write_fd(2, rest) {
                Ok(0) | Err(_) => return,
                Ok(n) => rest = &rest[n..],
            }
        }
    }
}

/// Standard input or another descriptor the reader doesn't own (and never
/// closes), read without Rust's buffering.
pub struct FdReader(pub i32);

impl Read for FdReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        read_fd(self.0, buf)
    }
}

/// util.c's `jq_realpath` without its fallback: `realpath`, or on Windows
/// `_fullpath`, which makes a path absolute without resolving links or
/// needing it to exist (as `std::path::absolute` does). `None` if it fails.
pub fn realpath(path: &OsStr) -> Option<OsString> {
    #[cfg(unix)]
    let full = std::fs::canonicalize(path);
    #[cfg(windows)]
    let full = std::path::absolute(path);
    full.ok().map(std::path::PathBuf::into_os_string)
}

/// Whether `dirname` splits at byte `b`: `/`, and on Windows (MinGW's
/// `dirname`) `\` too.
pub fn is_separator(b: u8) -> bool {
    b == b'/' || (cfg!(windows) && b == b'\\')
}

/// util.c's `get_home`: `$HOME`, and on Windows `%USERPROFILE%`, then
/// `%HOMEDRIVE%%HOMEPATH%`.
pub fn home_dir() -> Option<OsString> {
    if let Some(home) = std::env::var_os("HOME") {
        return Some(home);
    }
    #[cfg(windows)]
    {
        if let Some(home) = std::env::var_os("USERPROFILE") {
            return Some(home);
        }
        if let Some(path) = std::env::var_os("HOMEPATH") {
            let mut home = std::env::var_os("HOMEDRIVE").unwrap_or_default();
            home.push(path);
            return Some(home);
        }
    }
    None
}

#[cfg(windows)]
mod windows {
    use std::ffi::{OsStr, OsString};
    use std::io::{self, Read};
    use std::os::windows::ffi::OsStrExt as _;

    use libc::{c_int, c_uint, c_void, wchar_t};

    type InvalidParameterHandler =
        unsafe extern "C" fn(*const wchar_t, *const wchar_t, *const wchar_t, c_uint, usize);

    unsafe extern "C" {
        pub(super) fn _errno() -> *mut c_int;
        fn _setmode(fd: c_int, mode: c_int) -> c_int;
        fn _set_invalid_parameter_handler(
            handler: Option<InvalidParameterHandler>,
        ) -> Option<InvalidParameterHandler>;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleMode(console: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(console: *mut c_void, mode: u32) -> i32;
        fn WriteConsoleW(
            console: *mut c_void,
            buf: *const u16,
            len: u32,
            written: *mut u32,
            reserved: *mut c_void,
        ) -> i32;
    }

    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 4;

    unsafe extern "C" fn ignore_invalid_parameter(
        _: *const wchar_t,
        _: *const wchar_t,
        _: *const wchar_t,
        _: c_uint,
        _: usize,
    ) {
    }

    /// What jq's process has from the start on Windows: the environment's
    /// locale for the whole process (main.c's `setlocale(LC_ALL, "")`), and a
    /// C runtime that returns an error from a function given an invalid
    /// parameter (`strftime("%k")`, a `gmtime` out of range) instead of ending
    /// the process, as the MinGW runtime jq is built with has it.
    pub fn init() {
        // SAFETY: called once at start-up, before any other thread exists.
        unsafe {
            libc::setlocale(libc::LC_ALL, c"".as_ptr());
            _set_invalid_parameter_handler(Some(ignore_invalid_parameter));
        }
    }

    /// main.c's `-b`: standard input, output and error in binary mode.
    pub fn binary_stdio() {
        for fd in 0..3 {
            // SAFETY: changes the mode of a standard descriptor; a closed one
            // fails harmlessly.
            unsafe { _setmode(fd, libc::O_BINARY) };
        }
    }

    /// A file jq opens with `fopen(name, "r")` or `open(name, O_RDONLY)`: a C
    /// runtime descriptor in text mode, closed on drop.
    pub struct CrtFile(c_int);

    impl CrtFile {
        pub fn open(name: &OsStr) -> io::Result<CrtFile> {
            let wide: Vec<u16> = name.encode_wide().chain(Some(0)).collect();
            // SAFETY: `wide` is NUL-terminated.
            let fd = unsafe { libc::wopen(wide.as_ptr(), libc::O_RDONLY | libc::O_TEXT) };
            if fd < 0 {
                Err(super::errno_error())
            } else {
                Ok(CrtFile(fd))
            }
        }
    }

    impl Read for CrtFile {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            super::read_fd(self.0, buf)
        }
    }

    impl Drop for CrtFile {
        fn drop(&mut self) {
            // SAFETY: closing our own descriptor once.
            unsafe { libc::close(self.0) };
        }
    }

    /// Standard output when it is a console, written as jq's `put_buf` does:
    /// UTF-8 converted to UTF-16 for `WriteConsoleW`.
    pub struct Console {
        handle: *mut c_void,
        /// The start of a UTF-8 sequence that the last write cut off.
        pending: Vec<u8>,
        /// Whether output may be colored: `ANSICON` is set or the console
        /// took virtual-terminal sequences (main.c).
        pub color: bool,
    }

    impl Console {
        /// Standard output, if it is a console (the NUL device, which
        /// `isatty` also reports, isn't).
        pub fn stdout() -> Option<Console> {
            // SAFETY: fd 1's handle, or INVALID_HANDLE_VALUE, which
            // GetConsoleMode rejects.
            let handle = unsafe { libc::get_osfhandle(1) } as *mut c_void;
            let mut mode = 0;
            // SAFETY: a valid out-pointer.
            if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
                return None;
            }
            let color = std::env::var_os("ANSICON").is_some()
                // SAFETY: changes the mode of our own console handle.
                || unsafe { SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) } != 0;
            Some(Console {
                handle,
                pending: Vec::new(),
                color,
            })
        }

        /// Writes `bytes`, keeping an incomplete UTF-8 sequence at the end
        /// for the next write.
        pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.pending.extend_from_slice(bytes);
            let complete = match std::str::from_utf8(&self.pending) {
                Ok(_) => self.pending.len(),
                Err(e) if e.error_len().is_none() => e.valid_up_to(),
                Err(_) => self.pending.len(),
            };
            let text = String::from_utf8_lossy(&self.pending[..complete]);
            let wide: Vec<u16> = text.encode_utf16().collect();
            self.pending.drain(..complete);
            let mut rest = &wide[..];
            while !rest.is_empty() {
                let mut written = 0;
                let len = rest.len().min(1 << 20) as u32;
                // SAFETY: `rest` is valid for reads of `len` units.
                let ok = unsafe {
                    WriteConsoleW(
                        self.handle,
                        rest.as_ptr(),
                        len,
                        &mut written,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 || written == 0 {
                    return Err(io::Error::last_os_error());
                }
                rest = &rest[written as usize..];
            }
            Ok(())
        }
    }

    /// Unix's byte views of OS strings ([`std::os::unix::ffi::OsStrExt`]):
    /// on Windows, over std's encoding of them, WTF-8, which is the UTF-8 jq
    /// converts its wide `argv` to (`wmain`) for every valid string.
    pub trait OsStrExt {
        fn from_bytes(bytes: &[u8]) -> &Self;
        fn as_bytes(&self) -> &[u8];
    }

    impl OsStrExt for OsStr {
        fn from_bytes(bytes: &[u8]) -> &OsStr {
            match std::str::from_utf8(bytes) {
                Ok(s) => OsStr::new(s),
                // SAFETY: bytes that aren't UTF-8 only come from `as_bytes`
                // of an OS string (one with an unpaired surrogate), so they
                // are std's own encoding.
                Err(_) => unsafe { OsStr::from_encoded_bytes_unchecked(bytes) },
            }
        }

        fn as_bytes(&self) -> &[u8] {
            self.as_encoded_bytes()
        }
    }

    /// [`std::os::unix::ffi::OsStringExt`], as [`OsStrExt`].
    pub trait OsStringExt {
        fn from_vec(vec: Vec<u8>) -> Self;
        fn into_vec(self) -> Vec<u8>;
    }

    impl OsStringExt for OsString {
        fn from_vec(vec: Vec<u8>) -> OsString {
            match String::from_utf8(vec) {
                Ok(s) => OsString::from(s),
                // SAFETY: as in `OsStrExt::from_bytes`.
                Err(e) => unsafe { OsString::from_encoded_bytes_unchecked(e.into_bytes()) },
            }
        }

        fn into_vec(self) -> Vec<u8> {
            self.into_encoded_bytes()
        }
    }
}
