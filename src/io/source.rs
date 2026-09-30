//! Opening inputs: memory-mapped regular files, streams (stdin, pipes,
//! FIFOs, character devices, directories) and transparent decompression.
//!
//! This decides only *how* bytes are obtained. What jq does with them (its
//! `fgets` chunking, NUL truncation, line counting, error messages) is
//! emulated by [`super::reader`], which works the same for every kind of
//! source.
//!
//! # Giving memory back
//!
//! A memory-mapped file stays mapped whole (one mapping, advised
//! `MADV_SEQUENTIAL`, so the kernel reads ahead), but its pages are resident
//! once read, and without help the resident set grows to the size of the
//! file. So the reader *releases* what nothing can read anymore
//! ([`InputBytes::release`]; see `release_consumed` in [`super::reader`] for
//! when): the whole pages before that point are made inaccessible with
//! `mprotect(PROT_NONE)`, which on macOS takes them out of the process's
//! resident set (`madvise` doesn't, for file mappings), followed on Linux by
//! `madvise(MADV_DONTNEED)`, which does it there. The mapping itself stays,
//! so the address range is never reused, and a stale read of released bytes
//! faults instead of seeing other data.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// An input's bytes, possibly followed by readable padding (which lets
/// simdjson parse texts that end at the end of the input without a copy).
pub trait InputBytes: Send + Sync {
    /// The data. For bytes that can be released, only before the first
    /// [`InputBytes::release`].
    fn data(&self) -> &[u8];
    /// The data followed by whatever padding is readable after it (the
    /// padding's contents don't matter). The same restriction applies.
    fn padded(&self) -> &[u8] {
        self.data()
    }
    /// [`InputBytes::padded`] from data offset `from` on, which must not be
    /// before the offset the last [`InputBytes::release`] returned.
    fn padded_from(&self, from: usize) -> &[u8] {
        &self.padded()[from..]
    }
    /// Whether [`InputBytes::release`] gives memory back.
    fn releasable(&self) -> bool {
        false
    }
    /// Declares the data before `upto` dead: nothing reads it again (a read
    /// of the whole pages it covers faults from now on), and its memory is
    /// given back. Returns the offset the readable data starts at now: the
    /// largest `upto` so far (0 if these bytes can't be released).
    fn release(&self, upto: usize) -> usize {
        let _ = upto;
        0
    }
}

impl InputBytes for Vec<u8> {
    fn data(&self) -> &[u8] {
        self
    }
}

/// Bytes shared between the reader and parallel workers.
pub type SharedBytes = Arc<dyn InputBytes>;

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

/// A read-only private memory map of (part of) a file, followed by at
/// least one page of readable zeros: simdjson's padding, so that a text
/// ending at the end of the file is parsed in place. Its data can be
/// released from the start (see the module docs).
pub struct Mmap {
    map: *mut libc::c_void,
    /// The whole reservation, padding included.
    map_len: usize,
    /// Offset of the data within the mapping (the map starts page-aligned).
    skip: usize,
    len: usize,
    page: usize,
    /// Data offset before which nothing may be read ([`InputBytes::release`]).
    start: AtomicUsize,
    /// Bytes at the start of the mapping made inaccessible (whole pages).
    released: Mutex<usize>,
}

// SAFETY: the mapping is read-only; it is only ever made inaccessible, in
// ranges that nothing reads anymore (see `release`).
unsafe impl Send for Mmap {}
unsafe impl Sync for Mmap {}

/// Ranges of mappings made inaccessible so far, in the whole process (for
/// diagnostics and tests).
static RELEASES: AtomicU64 = AtomicU64::new(0);

/// How many times a range of a memory-mapped input was given back so far.
pub fn releases() -> u64 {
    RELEASES.load(Ordering::Relaxed)
}

/// The system's page size.
pub(crate) fn page_size() -> usize {
    // SAFETY: sysconf is always safe to call.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

impl Mmap {
    fn new(map: *mut libc::c_void, map_len: usize, skip: usize, len: usize) -> Mmap {
        Mmap {
            map,
            map_len,
            skip,
            len,
            page: page_size(),
            start: AtomicUsize::new(0),
            released: Mutex::new(0),
        }
    }

    /// An anonymous mapping holding a copy of `data` (which may be empty),
    /// starting `skip` bytes into its first page (`skip` is taken modulo the
    /// page size), set up and released exactly like a file's: for tests of
    /// what reads input when, where a stale read faults.
    #[cfg(unix)]
    pub(crate) fn copy_of(data: &[u8], skip: usize) -> Option<Mmap> {
        let page = page_size();
        let skip = skip % page;
        let map_len = (skip + data.len())
            .div_ceil(page)
            .checked_add(1)?
            .checked_mul(page)?;
        // SAFETY: a fresh anonymous reservation; checked for MAP_FAILED.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return None;
        }
        // SAFETY: the reservation is writable for map_len > skip + len bytes,
        // then made read-only.
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), (map as *mut u8).add(skip), data.len());
            libc::mprotect(map, map_len, libc::PROT_READ);
        }
        Some(Mmap::new(map, map_len, skip, data.len()))
    }

    /// Maps `len` (> 0) bytes of `fd` starting at `offset`, plus padding.
    /// `None` if mmap fails.
    #[cfg(unix)]
    fn map(fd: i32, offset: usize, len: usize) -> Option<Mmap> {
        let page = page_size();
        let start = offset / page * page;
        let skip = offset - start;
        let file_len = skip + len;
        // The file's pages (the last one zero-filled past the end of the
        // file), then a page of anonymous zeros: mapping the file itself
        // further would fault (SIGBUS) past its last page.
        let map_len = file_len.div_ceil(page).checked_add(1)?.checked_mul(page)?;
        // SAFETY: a fresh anonymous read-only reservation; checked for
        // MAP_FAILED below.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_len,
                libc::PROT_READ,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return None;
        }
        // SAFETY: replaces the start of our own reservation with a
        // read-only private mapping of an open fd.
        let file = unsafe {
            libc::mmap(
                map,
                file_len,
                libc::PROT_READ,
                libc::MAP_PRIVATE | libc::MAP_FIXED,
                fd,
                start as libc::off_t,
            )
        };
        if file == libc::MAP_FAILED {
            // SAFETY: unmapping our own reservation.
            unsafe { libc::munmap(map, map_len) };
            return None;
        }
        // SAFETY: advice on our own mapping.
        unsafe { libc::madvise(map, file_len, libc::MADV_SEQUENTIAL) };
        Some(Mmap::new(map, map_len, skip, len))
    }
}

impl InputBytes for Mmap {
    fn data(&self) -> &[u8] {
        &self.padded()[..self.len]
    }

    fn padded(&self) -> &[u8] {
        debug_assert_eq!(
            self.start.load(Ordering::Relaxed),
            0,
            "the whole input, after some of it was released"
        );
        self.padded_from(0)
    }

    fn padded_from(&self, from: usize) -> &[u8] {
        debug_assert!(
            from >= self.start.load(Ordering::Relaxed) && from <= self.len,
            "reading released input"
        );
        // SAFETY: the mapping has map_len bytes, readable from skip + start
        // on for the lifetime of self (release only protects bytes before
        // start), and skip + from <= skip + len < map_len.
        unsafe {
            std::slice::from_raw_parts(
                (self.map as *const u8).add(self.skip + from),
                self.map_len - self.skip - from,
            )
        }
    }

    fn releasable(&self) -> bool {
        true
    }

    fn release(&self, upto: usize) -> usize {
        let upto = upto.min(self.len);
        let mut released = self
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let start = self.start.load(Ordering::Relaxed);
        if upto <= start {
            return start;
        }
        self.start.store(upto, Ordering::Relaxed);
        // The whole pages before `upto` (the rest of its page stays).
        let end = (self.skip + upto) / self.page * self.page;
        if end > *released {
            // SAFETY: [released, end) is part of our mapping, and nothing
            // reads it anymore (the caller's promise). A failure leaves the
            // pages as they were, which is harmless.
            unsafe {
                let p = (self.map as *mut u8).add(*released).cast();
                libc::mprotect(p, end - *released, libc::PROT_NONE);
                #[cfg(target_os = "linux")]
                libc::madvise(p, end - *released, libc::MADV_DONTNEED);
            }
            *released = end;
            RELEASES.fetch_add(1, Ordering::Relaxed);
        }
        upto
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
