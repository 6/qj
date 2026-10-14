//! The oracle for the fast reader: jq 1.8.2's `util.c` input loop
//! (`jq_util_input_read_more` / `jq_util_input_next_input`) ported line by
//! line over in-memory inputs, with `fgets` of at most 4091 bytes into a
//! 4096-byte buffer (memset to 0xff first, as jq does), then `fread` of the
//! rest of a UTF-8 sequence the chunk cut, and jq's parser port. Nothing
//! here is shared with `crate::io::reader`.

use std::ffi::OsString;

use super::MemFile;
use crate::io::source::strerror;
use crate::jq::value::unicode::utf8_backtrack;
use crate::jq::value::{Array, Error, ParseFlags, Parser, Str, Value};

/// `sizeof(state->buf)`.
const BUF: usize = 4096;
/// `max_gets_len`: `sizeof(state->buf) - max_utf8_len`.
const MAX_GETS_LEN: usize = BUF - 4;

struct RefFile {
    data: Vec<u8>,
    pos: usize,
    eof: bool,
    error: bool,
    errno: i32,
    read_error: Option<i32>,
    is_stdin: bool,
}

impl RefFile {
    /// The stream runs out: `feof`, or the read error.
    fn hit_end(&mut self) {
        match self.read_error {
            Some(code) => {
                self.error = true;
                self.errno = code;
            }
            None => self.eof = true,
        }
    }

    /// `fgets(buf, size, f)`: `None` for NULL.
    fn fgets(&mut self, buf: &mut [u8], size: usize) -> Option<()> {
        if self.pos == self.data.len() {
            self.hit_end();
            return None;
        }
        let rest = &self.data[self.pos..];
        let lim = rest.len().min(size - 1);
        let n = match memchr::memchr(b'\n', &rest[..lim]) {
            Some(i) => i + 1,
            None => lim,
        };
        buf[..n].copy_from_slice(&rest[..n]);
        buf[n] = 0;
        self.pos += n;
        if self.pos == self.data.len() && buf[n - 1] != b'\n' && n < size - 1 {
            // fgets tried to read past the data.
            self.hit_end();
        }
        Some(())
    }

    /// `fread(buf, 1, buf.len(), f)`: how many bytes it read.
    fn fread(&mut self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        if n < buf.len() {
            self.hit_end();
        }
        n
    }
}

/// `struct jq_util_input_state` over in-memory inputs.
pub(crate) struct RefInput {
    files: Vec<(OsString, MemFile)>,
    names: Vec<OsString>,
    curr_file: usize,
    current: Option<RefFile>,
    /// jq never closes stdin: a second `-` continues where the first ended.
    stdin: Option<RefFile>,
    parser: Option<Parser>,
    slurped: Option<Value>,
    buf: [u8; BUF],
    buf_valid_len: usize,
    pub(crate) failures: usize,
    pub(crate) current_filename: Option<Str>,
    pub(crate) current_line: u64,
    pub(crate) messages: Vec<u8>,
}

impl RefInput {
    pub(crate) fn new(
        names: &[&str],
        files: Vec<(OsString, MemFile)>,
        raw: bool,
        slurp: bool,
        flags: ParseFlags,
    ) -> RefInput {
        RefInput {
            files,
            names: names.iter().map(OsString::from).collect(),
            curr_file: 0,
            current: None,
            stdin: None,
            parser: (!raw).then(|| Parser::new(flags)),
            slurped: match (slurp, raw) {
                (true, true) => Some(Value::String(Str::new())),
                (true, false) => Some(Value::Array(Array::new())),
                _ => None,
            },
            buf: [0; BUF],
            buf_valid_len: 0,
            failures: 0,
            current_filename: None,
            current_line: 0,
            messages: Vec::new(),
        }
    }

    fn open(&mut self, name: &OsString) -> Option<RefFile> {
        let file = self
            .files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, f)| f.clone());
        let (data, read_error) = match file {
            None => {
                self.open_failed(name, libc::ENOENT);
                return None;
            }
            Some(MemFile::Missing(code)) => {
                self.open_failed(name, code);
                return None;
            }
            Some(MemFile::Data(d)) => (d, None),
            Some(MemFile::ReadError(d, code)) => (d, Some(code)),
        };
        Some(RefFile {
            data,
            pos: 0,
            eof: false,
            error: false,
            errno: 0,
            read_error,
            is_stdin: false,
        })
    }

    fn open_failed(&mut self, name: &OsString, code: i32) {
        // fprinter
        let e = std::io::Error::from_raw_os_error(code);
        self.messages
            .extend_from_slice(b"jq: error: Could not open file ");
        self.messages
            .extend_from_slice(crate::io::source::os_bytes(name));
        self.messages.extend_from_slice(b": ");
        self.messages.extend_from_slice(&strerror(&e));
        self.messages.push(b'\n');
        self.failures += 1;
    }

    /// `jq_util_input_read_more`.
    fn read_more(&mut self) -> bool {
        let done = match &self.current {
            None => true,
            Some(f) => f.eof || f.error,
        };
        if done {
            if let Some(f) = &self.current
                && f.error
            {
                let e = std::io::Error::from_raw_os_error(f.errno);
                self.messages.extend_from_slice(b"jq: error: ");
                self.messages.extend_from_slice(&strerror(&e));
                self.messages.push(b'\n');
            }
            if let Some(mut f) = self.current.take()
                && f.is_stdin
            {
                // clearerr(stdin); we don't fclose(stdin)
                f.eof = false;
                f.error = false;
                self.stdin = Some(f);
            }
            if self.curr_file < self.names.len() {
                let name = self.names[self.curr_file].clone();
                self.curr_file += 1;
                self.current_line = 0;
                if name == "-" {
                    let stdin = match self.stdin.take() {
                        Some(s) => Some(s),
                        None => self.open(&name).map(|mut f| {
                            f.is_stdin = true;
                            f
                        }),
                    };
                    self.current = stdin;
                    self.current_filename = Some(Str::from("<stdin>"));
                } else {
                    self.current = self.open(&name);
                    self.current_filename =
                        Some(Str::from_bytes(crate::io::source::os_bytes(&name)));
                }
            }
        }

        self.buf[0] = 0;
        self.buf_valid_len = 0;
        if let Some(f) = &mut self.current {
            self.buf.fill(0xff);
            let res = f.fgets(&mut self.buf, MAX_GETS_LEN);
            if res.is_none() {
                self.buf[0] = 0;
                if f.error {
                    self.failures += 1;
                }
            } else {
                let p = memchr::memchr(b'\n', &self.buf[..MAX_GETS_LEN]);
                if p.is_some() {
                    self.current_line += 1;
                }
                self.buf_valid_len = match p {
                    None if f.eof => {
                        let mut i = MAX_GETS_LEN - 1;
                        while i > 0 && self.buf[i] != 0 {
                            i -= 1;
                        }
                        i
                    }
                    None => {
                        let mut len = MAX_GETS_LEN - 1;
                        let mut missing = 0;
                        if utf8_backtrack(&self.buf[..len], len - 1, 0, Some(&mut missing))
                            .is_some()
                            && missing > 0
                        {
                            len += f.fread(&mut self.buf[len..len + missing]);
                        }
                        len
                    }
                    Some(p) => p + 1,
                };
            }
        }
        self.curr_file == self.names.len() && self.current.is_none()
    }

    /// `jq_util_input_next_input`.
    pub(crate) fn next_input(&mut self) -> Option<Result<Value, Error>> {
        let mut value: Option<Result<Value, Error>> = None; // jv_invalid()
        let mut is_last = false;
        loop {
            if self.parser.is_none() {
                // Raw input
                is_last = self.read_more();
                if self.buf_valid_len != 0 {
                    let chunk = &self.buf[..self.buf_valid_len];
                    if let Some(Value::String(s)) = &mut self.slurped {
                        s.push_bytes(chunk);
                    } else {
                        let mut v = match value.take() {
                            Some(Ok(Value::String(s))) => s,
                            _ => Str::new(),
                        };
                        if chunk[chunk.len() - 1] == b'\n' {
                            // whole line
                            v.push_bytes(&chunk[..chunk.len() - 1]);
                            return Some(Ok(Value::String(v)));
                        }
                        v.push_bytes(chunk);
                        value = Some(Ok(Value::String(v)));
                        self.buf[0] = 0;
                        self.buf_valid_len = 0;
                    }
                }
            } else {
                let parser = self.parser.as_mut().unwrap();
                if parser.remaining() == 0 {
                    is_last = self.read_more();
                    let parser = self.parser.as_mut().unwrap();
                    parser.set_buf(&self.buf[..self.buf_valid_len], !is_last);
                }
                let parser = self.parser.as_mut().unwrap();
                value = parser.next();
                if let Some(Value::Array(a)) = &mut self.slurped {
                    match value.take() {
                        Some(Ok(v)) => a.push(v),
                        Some(Err(e)) => return Some(Err(e)), // Not slurped parsed input
                        None => {}
                    }
                } else if value.is_some() {
                    return value;
                }
            }
            if is_last {
                break;
            }
        }
        if let Some(s) = self.slurped.take() {
            return Some(Ok(s));
        }
        value
    }

    pub(crate) fn filename_value(&self) -> Value {
        match &self.current_filename {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        }
    }
}
