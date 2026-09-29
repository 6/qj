//! Opening inputs: memory-mapped regular files, streams (stdin, pipes,
//! FIFOs, character devices, directories) and transparent decompression.
//!
//! This decides only *how* bytes are obtained. What jq does with them (its
//! `fgets` chunking, NUL truncation, line counting, error messages) is
//! emulated by [`super::reader`], which works the same for every kind of
//! source.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::sync::Arc;

/// Bytes shared between the reader and parallel workers.
pub type SharedBytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// An opened input.
pub enum Opened {
    /// The whole content, available at once (a memory-mapped file or bytes
    /// in memory).
    Whole(SharedBytes),
    /// Content that arrives incrementally. `fd`, when present, lets the
    /// reader check whether more data is available without blocking.
    Stream {
        reader: Box<dyn Read>,
        fd: Option<i32>,
    },
}

impl Opened {
    /// Whole content from owned bytes.
    pub fn bytes(data: Vec<u8>) -> Opened {
        Opened::Whole(Arc::new(data))
    }
}

/// Opens inputs by name. The reader asks for each input when jq would
/// `fopen` it (lazily, in order); `"-"` is standard input.
pub trait Opener {
    fn open(&mut self, name: &OsStr) -> io::Result<Opened>;
}

/// Serves named inputs from memory (for tests, fuzzing and embedding).
/// Unknown names fail to open with `ENOENT`.
#[derive(Default)]
pub struct MemoryOpener {
    files: Vec<(OsString, Result<SharedBytes, i32>)>,
}

impl MemoryOpener {
    pub fn new() -> MemoryOpener {
        MemoryOpener::default()
    }

    /// Adds an input (`"-"` for standard input).
    pub fn add(&mut self, name: impl Into<OsString>, data: impl Into<Vec<u8>>) -> &mut Self {
        self.files
            .push((name.into(), Ok(Arc::new(data.into()) as SharedBytes)));
        self
    }

    /// Adds an input that fails to open with `errno`.
    pub fn add_error(&mut self, name: impl Into<OsString>, errno: i32) -> &mut Self {
        self.files.push((name.into(), Err(errno)));
        self
    }
}

impl Opener for MemoryOpener {
    fn open(&mut self, name: &OsStr) -> io::Result<Opened> {
        match self.files.iter().find(|(n, _)| n == name) {
            Some((_, Ok(data))) => Ok(Opened::Whole(data.clone())),
            Some((_, Err(errno))) => Err(io::Error::from_raw_os_error(*errno)),
            None => Err(io::Error::from_raw_os_error(2)), // ENOENT
        }
    }
}

/// The default opener: the file system and standard input.
///
/// * `-` is standard input; when it is a regular file (`< file`) it is
///   memory-mapped from its current offset.
/// * Names ending in `.gz`/`.gzip` or `.zst`/`.zstd` are decompressed as a
///   stream (a qj extension).
/// * Other regular files are memory-mapped (unless `QJ_NO_MMAP` is set).
/// * Anything else that opens (pipes, FIFOs, devices, and directories, whose
///   reads fail with `EISDIR` exactly as jq's `fgets` does) is a stream.
#[derive(Default)]
pub struct FsOpener {
    _private: (),
}

impl Opener for FsOpener {
    fn open(&mut self, name: &OsStr) -> io::Result<Opened> {
        if name == "-" {
            return open_stdin();
        }
        let file = File::open(name)?;
        let lossy = name.to_string_lossy();
        if crate::decompress::is_compressed(&lossy) {
            return open_compressed(file, &lossy);
        }
        open_file(file)
    }
}

fn open_compressed(file: File, name: &str) -> io::Result<Opened> {
    let buffered = io::BufReader::with_capacity(256 * 1024, file);
    let reader: Box<dyn Read> = if name.ends_with(".gz") || name.ends_with(".gzip") {
        // Concatenated gzip members decompress as one stream, like gzip(1).
        Box::new(flate2::read::MultiGzDecoder::new(buffered))
    } else {
        Box::new(zstd::stream::read::Decoder::with_buffer(buffered)?)
    };
    Ok(Opened::Stream { reader, fd: None })
}

#[cfg(unix)]
fn open_file(file: File) -> io::Result<Opened> {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::io::AsRawFd;
    let meta = file.metadata()?;
    let ft = meta.file_type();
    if ft.is_file() && std::env::var_os("QJ_NO_MMAP").is_none() {
        let len = meta.len() as usize;
        if len == 0 {
            return Ok(Opened::bytes(Vec::new()));
        }
        if let Some(map) = Mmap::map(file.as_raw_fd(), 0, len) {
            return Ok(Opened::Whole(Arc::new(map)));
        }
    }
    let fd = if ft.is_fifo() || ft.is_char_device() || ft.is_socket() {
        Some(file.as_raw_fd())
    } else {
        None
    };
    Ok(Opened::Stream {
        reader: Box::new(file),
        fd,
    })
}

#[cfg(not(unix))]
fn open_file(file: File) -> io::Result<Opened> {
    Ok(Opened::Stream {
        reader: Box::new(file),
        fd: None,
    })
}

/// A borrowed file descriptor read without Rust's `Stdin` buffering (the
/// reader does its own), and never closed: standard input.
struct FdReader(i32);

impl Read for FdReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            // SAFETY: the descriptor stays open for the reader's lifetime (fd
            // 0 for the process's); buf is writable for buf.len() bytes.
            let n = unsafe { libc::read(self.0, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        }
        #[cfg(not(unix))]
        {
            let _ = self.0;
            io::stdin().read(buf)
        }
    }
}

fn open_stdin() -> io::Result<Opened> {
    open_borrowed_fd(0)
}

/// Standard input (or another descriptor the reader doesn't own): when it
/// is a regular file (input redirected from a file), the rest of the file
/// from the current offset is mapped and the offset moved to the end as if
/// it had been read (so a second `-` sees EOF); otherwise it is streamed.
#[cfg(unix)]
pub(crate) fn open_borrowed_fd(fd: i32) -> io::Result<Opened> {
    // SAFETY: fstat with a valid out-pointer.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let is_file = unsafe { libc::fstat(fd, &mut st) } == 0
        && (st.st_mode & libc::S_IFMT) == libc::S_IFREG
        && std::env::var_os("QJ_NO_MMAP").is_none();
    if is_file {
        // SAFETY: lseek on an open descriptor.
        let offset = unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) };
        let len = st.st_size as usize;
        if offset >= 0 && (offset as usize) <= len {
            let offset = offset as usize;
            if offset == len {
                return Ok(Opened::bytes(Vec::new()));
            }
            if let Some(map) = Mmap::map(fd, offset, len - offset) {
                // SAFETY: lseek on an open descriptor.
                unsafe { libc::lseek(fd, 0, libc::SEEK_END) };
                return Ok(Opened::Whole(Arc::new(map)));
            }
        }
    }
    Ok(Opened::Stream {
        reader: Box::new(FdReader(fd)),
        fd: Some(fd),
    })
}

#[cfg(not(unix))]
pub(crate) fn open_borrowed_fd(fd: i32) -> io::Result<Opened> {
    Ok(Opened::Stream {
        reader: Box::new(FdReader(fd)),
        fd: None,
    })
}

/// A read-only private memory map of (part of) a file.
pub struct Mmap {
    map: *mut libc::c_void,
    map_len: usize,
    /// Offset of the data within the mapping (the map starts page-aligned).
    skip: usize,
    len: usize,
}

// SAFETY: the mapping is read-only and never remapped while shared.
unsafe impl Send for Mmap {}
unsafe impl Sync for Mmap {}

impl Mmap {
    /// Maps `len` bytes of `fd` starting at `offset`. `None` if mmap fails.
    #[cfg(unix)]
    fn map(fd: i32, offset: usize, len: usize) -> Option<Mmap> {
        // SAFETY: sysconf is always safe to call.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let start = offset / page * page;
        let skip = offset - start;
        let map_len = skip + len;
        // SAFETY: a fresh read-only private mapping of an open fd; checked
        // for MAP_FAILED below.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                fd,
                start as libc::off_t,
            )
        };
        if map == libc::MAP_FAILED {
            return None;
        }
        // SAFETY: advice on our own mapping.
        unsafe { libc::madvise(map, map_len, libc::MADV_SEQUENTIAL) };
        Some(Mmap {
            map,
            map_len,
            skip,
            len,
        })
    }
}

impl AsRef<[u8]> for Mmap {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: the mapping covers skip + len readable bytes for the
        // lifetime of self.
        unsafe { std::slice::from_raw_parts((self.map as *const u8).add(self.skip), self.len) }
    }
}

impl Drop for Mmap {
    fn drop(&mut self) {
        // SAFETY: unmapping our own mapping once.
        unsafe { libc::munmap(self.map, self.map_len) };
    }
}

/// C's `strerror(errno)` text for an I/O error (jq prints it verbatim),
/// falling back to the error's own description when it has no errno.
pub fn strerror(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => {
            // SAFETY: strerror returns a pointer to a NUL-terminated static
            // (or thread-local) string.
            let s = unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) };
            s.to_string_lossy().into_owned()
        }
        None => e.to_string(),
    }
}

/// Messages jq's input layer prints to stderr (`util.c`).
#[derive(Debug)]
pub enum InputMessage {
    /// `jq: error: Could not open file <name>: <strerror>` (`fprinter`).
    OpenFailed { name: OsString, error: io::Error },
    /// `jq: error: <strerror>`: a read error ended the previous input.
    ReadFailed { error: io::Error },
}

impl InputMessage {
    /// The message as jq prints it, with `prog` in place of `jq`, including
    /// the trailing newline.
    pub fn render(&self, prog: &str) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            InputMessage::OpenFailed { name, error } => {
                out.extend_from_slice(format!("{prog}: error: Could not open file ").as_bytes());
                out.extend_from_slice(os_bytes(name));
                out.extend_from_slice(format!(": {}\n", strerror(error)).as_bytes());
            }
            InputMessage::ReadFailed { error } => {
                out.extend_from_slice(format!("{prog}: error: {}\n", strerror(error)).as_bytes());
            }
        }
        out
    }
}

/// The raw bytes of an OS string (argv bytes on Unix).
pub fn os_bytes(s: &OsStr) -> &[u8] {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        s.as_bytes()
    }
    #[cfg(not(unix))]
    {
        s.to_str().map(str::as_bytes).unwrap_or(b"?")
    }
}

/// Whether more bytes can be read from `fd` right now without blocking.
pub(crate) fn readable_now(fd: i32) -> bool {
    #[cfg(unix)]
    {
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll on one valid pollfd with a zero timeout.
        let r = unsafe { libc::poll(&mut p, 1, 0) };
        r > 0 && (p.revents & (libc::POLLIN | libc::POLLHUP)) != 0
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        false
    }
}
