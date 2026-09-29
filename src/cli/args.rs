//! Port of jq 1.8.1 `src/main.c`: command-line argument handling.
//!
//! [`parse`] is main.c's option loop. It walks argv the way jq does, so a
//! command line is accepted or rejected at the same argument, with the same
//! diagnostic and exit status. The rest of main.c that depends only on the
//! options is here too, for the binary to call in jq's order:
//!
//! 1. [`parse`], inside [`with_environment_locale`]: jq calls
//!    `setlocale(LC_ALL, "")` first, and its loop uses `isalpha`, `isspace`
//!    and `strtol`, so which arguments look like options depends on the
//!    locale.
//! 2. [`Options::dumpopts`]: the terminal and `NO_COLOR` defaults, then `-S`,
//!    `-a`, `-C`, `-M`.
//! 3. [`jq_colors`]: when `JQ_COLORS` is invalid, print [`JQ_COLORS_WARNING`]
//!    and keep the default colors.
//! 4. [`Options::program_or_default`]: without a program, jq prints its short
//!    usage and exits 2 ([`ArgError::NoProgram`]).
//! 5. With `-f`, [`load_program`].
//! 6. Compile with the variables from [`Options::program_arguments`].
//!
//! Evaluation, input reading and printing are elsewhere.
//!
//! Arguments are bytes, as in C. Values jq turns into strings with
//! `jv_string` (which replaces invalid UTF-8 with U+FFFD) stay bytes here
//! ([`ArgValue::Text`]); the caller converts them with its own string type.
//!
//! # qj extensions
//!
//! `--threads N` (or `--threads=N`), `--jsonl` and `--debug-timing` are long
//! options jq doesn't have. They are tried only after every jq option failed
//! to match, where jq reports "Unknown option", so every command line jq
//! accepts parses the same way in qj. Glob expansion of input files
//! ([`expand_file_globs`]) replaces a file argument only when it doesn't
//! exist, contains a glob metacharacter and matches at least one path;
//! otherwise the argument is kept, and it fails to open exactly as in jq.

use std::ffi::{CStr, CString, OsStr};
use std::io::Read;
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// jv.h `enum jv_print_flags`: the printer flags main.c collects in
/// `dumpopts`.
pub mod print_flags {
    pub const PRETTY: u32 = 1;
    pub const ASCII: u32 = 2;
    pub const COLOR: u32 = 4;
    pub const SORTED: u32 = 8;
    pub const INVALID: u32 = 16;
    pub const REFCOUNT: u32 = 32;
    pub const TAB: u32 = 64;
    pub const ISATTY: u32 = 128;
    pub const SPACE0: u32 = 256;
    pub const SPACE1: u32 = 512;
    pub const SPACE2: u32 = 1024;

    /// jv.h `JV_PRINT_INDENT_FLAGS(n)`: tabs for `n` outside 0..=7, else `n`
    /// spaces. 0 is still pretty-printed (newlines, no indentation).
    pub const fn indent_flags(n: i64) -> u32 {
        if n < 0 || n > 7 {
            TAB | PRETTY
        } else {
            ((n as u32) << 8) | PRETTY
        }
    }

    /// The indentation width of pretty output without `TAB`.
    pub const fn indent_width(flags: u32) -> u32 {
        (flags & (SPACE0 | SPACE1 | SPACE2)) >> 8
    }
}

/// jv.h `JV_PARSE_*` parser flags.
pub mod parse_flags {
    pub const SEQ: u32 = 1;
    pub const STREAMING: u32 = 2;
    pub const STREAM_ERRORS: u32 = 4;
}

/// jq.h `JQ_DEBUG_*` flags (main.c's `jq_flags`).
pub mod debug_flags {
    pub const TRACE: u32 = 1;
    pub const TRACE_DETAIL: u32 = 2;
    pub const TRACE_ALL: u32 = TRACE | TRACE_DETAIL;
}

use print_flags::{PRETTY, TAB, indent_flags};

/// The side effects main.c's option loop needs from the value layer.
pub trait ArgHost {
    type Value;

    /// `jv_parse(text)`: exactly one JSON text, optionally surrounded by
    /// whitespace. Used by `--argjson` and `--jsonargs`; jq only reports that
    /// the text is invalid, so the message isn't shown.
    fn parse_json(&mut self, text: &[u8]) -> Result<Self::Value, String>;

    /// The parsing half of `jv_load_file(file, 0)` (`--slurpfile`): every
    /// JSON text in `data`, as an array, or the parser's message, which jq
    /// shows as "Bad JSON in --slurpfile NAME FILE: MESSAGE".
    fn slurp_json(&mut self, data: &[u8]) -> Result<Self::Value, String>;
}

/// A named (`$name`) or positional (`$ARGS.positional`) argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgValue<V> {
    /// Bytes that become a jq string: `--arg`, `--args`, `--rawfile`. jq
    /// builds these with `jv_string`, replacing invalid UTF-8 with U+FFFD.
    Text(Vec<u8>),
    /// A value from the [`ArgHost`]: `--argjson`, `--jsonargs`, `--slurpfile`.
    Json(V),
}

/// Which option defines a named argument, for its messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedOption {
    Arg,
    Argjson,
    Rawfile,
    Slurpfile,
}

impl NamedOption {
    fn name(self) -> &'static str {
        match self {
            NamedOption::Arg => "arg",
            NamedOption::Argjson => "argjson",
            NamedOption::Rawfile => "rawfile",
            NamedOption::Slurpfile => "slurpfile",
        }
    }

    /// The second parameter's name in "takes two parameters (e.g. ...)".
    fn example(self) -> &'static str {
        match self {
            NamedOption::Arg => "value",
            NamedOption::Argjson => "text",
            NamedOption::Rawfile | NamedOption::Slurpfile => "filename",
        }
    }
}

/// Everything main.c's option loop collects.
///
/// The booleans are main.c's `options` bits (and `parser_flags`), named
/// after jq's constants; like jq, `--raw-output0` sets `raw_output`,
/// `raw_no_lf` and `raw_output0`, and `--stream-errors` sets `stream` too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options<V> {
    /// `argv[0]`; `$ORIGIN` is its directory ([`Options::jq_origin`]).
    pub argv0: Vec<u8>,
    /// The first non-option argument: the program text, or with `-f` the
    /// program file.
    pub program: Option<Vec<u8>>,
    /// Input files, in order. `-` is stdin. jq reads stdin when there are
    /// none.
    pub files: Vec<Vec<u8>>,
    /// `$ARGS.positional`, from arguments after `--args`/`--jsonargs`.
    pub positional: Vec<ArgValue<V>>,
    /// `--arg`, `--argjson`, `--rawfile`, `--slurpfile` in order. The first
    /// definition of a name wins; later ones are skipped without parsing
    /// their JSON or reading their file.
    pub named: Vec<(Vec<u8>, ArgValue<V>)>,
    /// `-L` directories (made absolute with `realpath` when they exist), or
    /// `None` for jq's default list ([`Options::library_paths`]).
    pub lib_search_paths: Option<Vec<Vec<u8>>>,
    /// `dumpopts` as the loop leaves it: only indentation (`-c`, `--tab`,
    /// `--indent`). [`Options::dumpopts`] adds colors, `-S` and `-a`.
    pub dumpopts: u32,
    pub slurp: bool,
    pub raw_input: bool,
    /// `-n` (`PROVIDE_NULL`).
    pub null_input: bool,
    pub raw_output: bool,
    pub raw_output0: bool,
    /// `-j` and `--raw-output0`: no newline after each output.
    pub raw_no_lf: bool,
    pub ascii_output: bool,
    /// `-C`
    pub color_output: bool,
    /// `-M`, which wins over `-C` whatever the order.
    pub no_color_output: bool,
    pub sorted_output: bool,
    /// `-f`: [`Options::program`] names a file.
    pub from_file: bool,
    pub unbuffered_output: bool,
    pub exit_status: bool,
    pub seq: bool,
    /// `--debug-dump-disasm`
    pub dump_disasm: bool,
    /// `--stream` or `--stream-errors` (`JV_PARSE_STREAMING`).
    pub stream: bool,
    /// `--stream-errors` (`JV_PARSE_STREAM_ERRORS`).
    pub stream_errors: bool,
    /// `--debug-trace` / `--debug-trace=all`: [`debug_flags`].
    pub jq_flags: u32,
    /// qj: `--threads N`.
    pub threads: Option<usize>,
    /// qj: `--jsonl`, read input as NDJSON.
    pub jsonl: bool,
    /// qj: `--debug-timing` (hidden).
    pub debug_timing: bool,
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action<V> {
    /// Run a program (the normal case).
    Run(Options<V>),
    /// `-h`/`--help`: usage on stdout, exit 0.
    Help,
    /// `-V`/`--version`: exit 0.
    Version,
    /// `--build-configuration`: exit 0.
    BuildConfiguration,
    /// `--run-tests`: jq's test runner, given the options before it and every
    /// argument after it.
    RunTests {
        options: Options<V>,
        args: Vec<Vec<u8>>,
    },
}

/// A diagnostic main.c prints before running anything. All of these exit 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    /// "Unknown option -%c": the byte of a short-option bundle that matched
    /// nothing.
    UnknownShortOption(u8),
    /// "Unknown option --%s"
    UnknownLongOption(Vec<u8>),
    /// "-L takes a parameter: ..." (jq prints this one without a prefix)
    LibraryPathMissing,
    /// "--indent takes one parameter"
    IndentMissing,
    /// "--indent takes a number between -1 and 7"
    IndentInvalid,
    /// "--%s takes two parameters (e.g. --%s varname %s)"
    MissingParameters(NamedOption),
    /// "invalid JSON text passed to --argjson"
    InvalidArgjson,
    /// "invalid JSON text passed to --jsonargs"
    InvalidJsonargs,
    /// "Bad JSON in --%s %s %s: %s": a `--rawfile`/`--slurpfile` file that
    /// couldn't be read or parsed. jq prints no usage hint after this one.
    BadFile {
        option: NamedOption,
        name: Vec<u8>,
        file: Vec<u8>,
        message: Vec<u8>,
    },
    /// No program and no default (see [`Options::program_or_default`]): jq
    /// prints its short usage. The usage text is qj's own.
    NoProgram,
    /// `-f` file that couldn't be read: jq prints the `jv_load_file` message.
    ProgramFile(Vec<u8>),
    /// qj: `--threads` without a count.
    ThreadsMissing,
    /// qj: `--threads` with something that isn't a count.
    ThreadsInvalid(Vec<u8>),
}

/// main.c `die()`, printed after most option errors. Verbatim, "jq" included:
/// qj's stderr must match jq's except for the leading program name.
pub const USAGE_HINT: &str = "Use jq --help for help with command-line options,\n\
                              or see the jq manpage, or online docs  at https://jqlang.org\n";

impl ArgError {
    /// The exit status jq uses (always 2: `die()`, `usage(2, 1)`, or
    /// `JQ_ERROR_SYSTEM`).
    pub fn exit_code(&self) -> i32 {
        2
    }

    /// The exact stderr bytes, with `prog` where jq prints "jq" at the start
    /// of its own messages.
    pub fn render(&self, prog: &str) -> Vec<u8> {
        let mut s: Vec<u8> = Vec::new();
        let mut line = |parts: &[&[u8]]| {
            s.extend_from_slice(prog.as_bytes());
            s.extend_from_slice(b": ");
            for p in parts {
                s.extend_from_slice(p);
            }
            s.push(b'\n');
        };
        match self {
            ArgError::UnknownShortOption(c) => line(&[b"Unknown option -", &[*c]]),
            ArgError::UnknownLongOption(t) => line(&[b"Unknown option --", t]),
            ArgError::LibraryPathMissing => s.extend_from_slice(
                b"-L takes a parameter: (e.g. -L /search/path or -L/search/path)\n",
            ),
            ArgError::IndentMissing => line(&[b"--indent takes one parameter"]),
            ArgError::IndentInvalid => line(&[b"--indent takes a number between -1 and 7"]),
            ArgError::MissingParameters(opt) => {
                let n = opt.name().as_bytes();
                line(&[
                    b"--",
                    n,
                    b" takes two parameters (e.g. --",
                    n,
                    b" varname ",
                    opt.example().as_bytes(),
                    b")",
                ])
            }
            ArgError::InvalidArgjson => line(&[b"invalid JSON text passed to --argjson"]),
            ArgError::InvalidJsonargs => line(&[b"invalid JSON text passed to --jsonargs"]),
            ArgError::BadFile {
                option,
                name,
                file,
                message,
            } => line(&[
                b"Bad JSON in --",
                option.name().as_bytes(),
                b" ",
                name,
                b" ",
                file,
                b": ",
                message,
            ]),
            ArgError::NoProgram => s.extend_from_slice(crate::cli::usage::short_usage().as_bytes()),
            ArgError::ProgramFile(message) => line(&[message]),
            ArgError::ThreadsMissing => {
                line(&[b"--threads takes one parameter (e.g. --threads 4)"])
            }
            ArgError::ThreadsInvalid(v) => line(&[
                b"--threads takes a number of threads (e.g. --threads 4), not '",
                v,
                b"'",
            ]),
        }
        if self.prints_usage_hint() {
            s.extend_from_slice(USAGE_HINT.as_bytes());
        }
        s
    }

    /// Whether jq follows the message with `die()`'s hint.
    fn prints_usage_hint(&self) -> bool {
        !matches!(
            self,
            ArgError::BadFile { .. } | ArgError::NoProgram | ArgError::ProgramFile(_)
        )
    }
}

impl<V> Options<V> {
    fn new(argv0: Vec<u8>) -> Self {
        Options {
            argv0,
            program: None,
            files: Vec::new(),
            positional: Vec::new(),
            named: Vec::new(),
            lib_search_paths: None,
            dumpopts: indent_flags(2),
            slurp: false,
            raw_input: false,
            null_input: false,
            raw_output: false,
            raw_output0: false,
            raw_no_lf: false,
            ascii_output: false,
            color_output: false,
            no_color_output: false,
            sorted_output: false,
            from_file: false,
            unbuffered_output: false,
            exit_status: false,
            seq: false,
            dump_disasm: false,
            stream: false,
            stream_errors: false,
            jq_flags: 0,
            threads: None,
            jsonl: false,
            debug_timing: false,
        }
    }

    fn has_named(&self, name: &[u8]) -> bool {
        self.named.iter().any(|(n, _)| n == name)
    }

    /// main.c after the loop: `dumpopts` with color on for a terminal unless
    /// `NO_COLOR` is set and non-empty, then `-S`, `-a`, `-C`, and `-M` (so
    /// `-M` beats `-C`, and `-C` beats `NO_COLOR`).
    pub fn dumpopts(&self, stdout_is_tty: bool, no_color_env: Option<&[u8]>) -> u32 {
        use print_flags::{ASCII, COLOR, ISATTY, SORTED};
        let mut d = self.dumpopts;
        if stdout_is_tty {
            d |= ISATTY | COLOR;
            if no_color_env.is_some_and(|v| !v.is_empty()) {
                d &= !COLOR;
            }
        }
        if self.sorted_output {
            d |= SORTED;
        }
        if self.ascii_output {
            d |= ASCII;
        }
        if self.color_output {
            d |= COLOR;
        }
        if self.no_color_output {
            d &= !COLOR;
        }
        d
    }

    /// The flags main.c gives `jv_parser_new` ([`parse_flags`]).
    pub fn parser_flags(&self) -> u32 {
        let mut f = 0;
        if self.stream {
            f |= parse_flags::STREAMING;
        }
        if self.stream_errors {
            f |= parse_flags::STREAM_ERRORS;
        }
        if self.seq {
            f |= parse_flags::SEQ;
        }
        f
    }

    /// The program argument, or `.` when there is none, no `-f`, and stdin or
    /// stdout isn't a terminal. `None` means [`ArgError::NoProgram`].
    pub fn program_or_default(&self, stdin_is_tty: bool, stdout_is_tty: bool) -> Option<&[u8]> {
        match &self.program {
            Some(p) => Some(p),
            None if !self.from_file && (!stdout_is_tty || !stdin_is_tty) => Some(b"."),
            None => None,
        }
    }

    /// The module search list (`JQ_LIBRARY_PATH`): the `-L` directories, or
    /// jq's default, which the linker expands later.
    pub fn library_paths(&self) -> Vec<Vec<u8>> {
        match &self.lib_search_paths {
            Some(paths) => paths.clone(),
            None => vec![
                b"~/.jq".to_vec(),
                b"$ORIGIN/../lib/jq".to_vec(),
                b"$ORIGIN/../lib".to_vec(),
            ],
        }
    }

    /// `JQ_ORIGIN` (`get_jq_origin`, `$ORIGIN` in the search list):
    /// `dirname(argv[0])`, not made absolute.
    pub fn jq_origin(&self) -> Vec<u8> {
        dirname(&self.argv0)
    }

    /// `PROGRAM_ORIGIN` (`get_prog_origin`, relative module paths): the
    /// program file's directory with `-f`, else the current directory, made
    /// absolute with `realpath`.
    pub fn program_origin(&self) -> Vec<u8> {
        match (&self.program, self.from_file) {
            (Some(file), true) => jq_realpath(&dirname(file)),
            _ => jq_realpath(b"."),
        }
    }

    /// The variables main.c passes to `jq_compile_args`: every named
    /// argument, except that `$ARGS` is always jq's own object (it replaces a
    /// user's `--arg ARGS`), plus `$JQ_BUILD_CONFIGURATION` unless the user
    /// defined it. `$ARGS.named` is [`Options::named`], unfiltered.
    ///
    /// The compiler still resolves `$ENV` before these, so `--arg ENV x`
    /// shows up in `$ARGS.named` but doesn't change `$ENV`.
    pub fn program_arguments(&self) -> Vec<ProgramArgument<'_, V>> {
        let mut vars: Vec<ProgramArgument<'_, V>> = self
            .named
            .iter()
            .filter(|(name, _)| name.as_slice() != b"ARGS")
            .map(|(name, value)| ProgramArgument::Named(name, value))
            .collect();
        vars.push(ProgramArgument::Args);
        if !self.has_named(b"JQ_BUILD_CONFIGURATION") {
            vars.push(ProgramArgument::BuildConfiguration);
        }
        vars
    }
}

/// A variable binding from [`Options::program_arguments`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramArgument<'a, V> {
    /// `$name` from `--arg`, `--argjson`, `--rawfile` or `--slurpfile`.
    Named(&'a [u8], &'a ArgValue<V>),
    /// `$ARGS`: `{"positional": [...], "named": {...}}`, from
    /// [`Options::positional`] and [`Options::named`] in order.
    Args,
    /// `$JQ_BUILD_CONFIGURATION`: the `--build-configuration` text.
    BuildConfiguration,
}

/// main.c `isoptish()`: starts with `-` followed by `-` or a letter. `-` alone
/// is an argument (stdin, or a program).
fn isoptish(text: &[u8]) -> bool {
    match text {
        [b'-', b'-', ..] => true,
        [b'-', c, ..] => c_isalpha(*c),
        _ => false,
    }
}

/// `isalpha((unsigned char)c)` in the calling thread's locale.
fn c_isalpha(c: u8) -> bool {
    // SAFETY: isalpha accepts any unsigned char value.
    unsafe { libc::isalpha(c_int::from(c)) != 0 }
}

/// main.c `isoption()`: in a bundle of short options, match and consume
/// `shortopt`; for a long option, match `longopt` exactly (no abbreviations,
/// no `=value`). `text` becomes `None` when nothing is left.
fn isoption(text: &mut Option<&[u8]>, shortopt: u8, longopt: &str, is_short: bool) -> bool {
    let Some(t) = *text else {
        return false;
    };
    if is_short {
        if shortopt != 0 && t.first() == Some(&shortopt) {
            let rest = &t[1..];
            *text = if rest.is_empty() { None } else { Some(rest) };
            return true;
        }
    } else if t == longopt.as_bytes() {
        *text = None;
        return true;
    }
    false
}

/// main.c's `--indent` check: `strtol(arg, &end, 10)` must consume the whole,
/// non-empty argument, which must not start with a space, and give -1..=7.
///
/// This calls the C library like jq does, because the locale matters: in a
/// UTF-8 locale on macOS, `strtol` skips a leading 0xA0 byte as a space while
/// jq's `isspace(*arg)` (a signed `char`) doesn't catch it, so jq accepts
/// `--indent $'\xa03'`.
fn parse_indent(arg: &[u8]) -> Option<i64> {
    // `isspace(*argv[i+1])` passes a plain `char`, sign-extended where `char`
    // is signed.
    let first = arg.first().copied().unwrap_or(0) as c_char;
    // SAFETY: isspace is defined for every `char` value on the platforms qj
    // supports (glibc and macOS index their tables from -128).
    if unsafe { libc::isspace(c_int::from(first)) } != 0 {
        return None;
    }
    let c = CString::new(arg).ok()?;
    let mut end: *mut c_char = std::ptr::null_mut();
    // SAFETY: `c` is NUL-terminated, and `end` is a valid out-pointer.
    let n = unsafe { libc::strtol(c.as_ptr(), &mut end, 10) };
    let consumed = end as usize - c.as_ptr() as usize;
    // `errno` (ERANGE) comes with LONG_MIN/LONG_MAX, which fail the range
    // check anyway.
    if consumed == 0 || consumed != arg.len() || !(-1..=7).contains(&n) {
        return None;
    }
    #[allow(clippy::useless_conversion)] // c_long is i32 on 32-bit targets
    Some(i64::from(n))
}

/// qj's `--threads` value.
fn parse_threads(arg: &[u8]) -> Result<usize, ArgError> {
    std::str::from_utf8(arg)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or_else(|| ArgError::ThreadsInvalid(arg.to_vec()))
}

/// Port of util.c `jq_realpath`: the canonical absolute path, or the path
/// unchanged when `realpath` fails (e.g. it doesn't exist).
pub fn jq_realpath(path: &[u8]) -> Vec<u8> {
    match std::fs::canonicalize(OsStr::from_bytes(path)) {
        Ok(p) => p.into_os_string().into_vec(),
        Err(_) => path.to_vec(),
    }
}

/// POSIX `dirname(3)`, as main.c uses it for `JQ_ORIGIN` and
/// `PROGRAM_ORIGIN`.
pub fn dirname(path: &[u8]) -> Vec<u8> {
    let trimmed = match path.iter().rposition(|&b| b != b'/') {
        Some(end) => &path[..=end],
        // Empty, or only slashes.
        None if path.is_empty() => return b".".to_vec(),
        None => return b"/".to_vec(),
    };
    match trimmed.iter().rposition(|&b| b == b'/') {
        None => b".".to_vec(),
        Some(slash) => match trimmed[..slash].iter().rposition(|&b| b != b'/') {
            Some(end) => trimmed[..=end].to_vec(),
            None => b"/".to_vec(),
        },
    }
}

/// `strerror(errnum)`.
fn strerror(errnum: i32) -> Vec<u8> {
    let mut buf = [0 as c_char; 512];
    // SAFETY: `buf` is writable for its length; strerror_r (the XSI version,
    // which the libc crate binds on glibc too) NUL-terminates it.
    let rc = unsafe { libc::strerror_r(errnum, buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return format!("Unknown error: {errnum}").into_bytes();
    }
    // SAFETY: NUL-terminated by strerror_r.
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_bytes().to_vec()
}

/// The I/O half of jv_file.c `jv_load_file`: the file's bytes, or jq's
/// message ("Could not open FILE: REASON", "Could not open FILE: It's a
/// directory", "Error reading from FILE").
pub fn load_file(path: &[u8]) -> Result<Vec<u8>, Vec<u8>> {
    let msg = |parts: &[&[u8]]| parts.concat();
    let mut file = match std::fs::File::open(OsStr::from_bytes(path)) {
        Ok(f) => f,
        Err(e) => {
            let reason = strerror(e.raw_os_error().unwrap_or(0));
            return Err(msg(&[b"Could not open ", path, b": ", &reason]));
        }
    };
    match file.metadata() {
        Ok(m) if !m.is_dir() => {}
        _ => return Err(msg(&[b"Could not open ", path, b": It's a directory"])),
    }
    let mut data = Vec::new();
    if file.read_to_end(&mut data).is_err() {
        return Err(msg(&[b"Error reading from ", path]));
    }
    Ok(data)
}

/// main.c's `-f` handling: read the program file (`jv_load_file(program,
/// 1)`), which `jq_compile_args` then reads up to the first NUL byte.
pub fn load_program(path: &[u8]) -> Result<Vec<u8>, ArgError> {
    let mut text = load_file(path).map_err(ArgError::ProgramFile)?;
    if let Some(nul) = text.iter().position(|&b| b == 0) {
        text.truncate(nul);
    }
    Ok(text)
}

/// jv_print.c's default colors: null, false, true, numbers, strings, arrays,
/// objects, object keys.
pub const DEFAULT_COLORS: [&str; 8] = [
    "\x1b[0;90m",
    "\x1b[0;39m",
    "\x1b[0;39m",
    "\x1b[0;39m",
    "\x1b[0;32m",
    "\x1b[1;39m",
    "\x1b[1;39m",
    "\x1b[1;34m",
];

/// What main.c prints (without a program-name prefix) when
/// `jq_set_colors(getenv("JQ_COLORS"))` fails. jq continues with the default
/// colors.
pub const JQ_COLORS_WARNING: &str = "Failed to set $JQ_COLORS\n";

/// Port of jv_print.c `jq_set_colors`: the color table for `JQ_COLORS`, or
/// `None` when it's invalid.
///
/// The value is up to eight `:`-separated fields of digits and `;`, each
/// becoming `ESC [ field m` for the kinds in [`DEFAULT_COLORS`] order. Kinds
/// without a field keep their default; an empty last field is ignored. jq
/// stops reading after the eighth field, so anything after it is accepted.
pub fn jq_colors(spec: &[u8]) -> Option<[Vec<u8>; 8]> {
    const LEN: usize = DEFAULT_COLORS.len();
    let is_code = |b: u8| b.is_ascii_digit() || b == b';';
    let mut fields: Vec<&[u8]> = Vec::new();
    let mut pos = 0;
    loop {
        let start = pos;
        while pos < spec.len() && is_code(spec[pos]) {
            pos += 1;
        }
        fields.push(&spec[start..pos]);
        if pos == spec.len() || fields.len() >= LEN {
            break;
        }
        if spec[pos] != b':' {
            return None;
        }
        pos += 1;
    }
    if fields.last().is_some_and(|f| f.is_empty()) {
        fields.pop();
    }
    let mut colors: [Vec<u8>; 8] = DEFAULT_COLORS.map(|c| c.as_bytes().to_vec());
    for (color, field) in colors.iter_mut().zip(&fields) {
        *color = [b"\x1b[", *field, b"m"].concat();
    }
    Some(colors)
}

/// Run `f` with the ctype functions of the calling thread in the locale jq
/// would use after `setlocale(LC_ALL, "")`. [`parse`] is meant to run inside
/// this: jq's option loop classifies bytes with `isalpha`/`isspace`, so on
/// macOS in a UTF-8 locale `jq -é` is an unknown option (exit 2), while in
/// the C locale it's a program (exit 3).
///
/// Only this thread and only `LC_CTYPE` are affected, and only for the
/// duration of `f`; like `setlocale`, an environment locale that can't be
/// loaded leaves the C locale in place.
pub fn with_environment_locale<R>(f: impl FnOnce() -> R) -> R {
    struct Restore {
        previous: libc::locale_t,
        ours: libc::locale_t,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: `previous` came from uselocale, and `ours` from
            // newlocale; `ours` is no longer in use after the switch back.
            unsafe {
                libc::uselocale(self.previous);
                libc::freelocale(self.ours);
            }
        }
    }
    // setlocale(LC_ALL, "") fails, changing nothing, when any category of the
    // environment's locale is unavailable; check the same way.
    // SAFETY: newlocale with a NUL-terminated name and a null base.
    let all = unsafe { libc::newlocale(libc::LC_ALL_MASK, c"".as_ptr(), std::ptr::null_mut()) };
    if all.is_null() {
        return f();
    }
    // SAFETY: `all` came from newlocale and isn't in use.
    unsafe { libc::freelocale(all) };
    // SAFETY: as above.
    let ours = unsafe { libc::newlocale(libc::LC_CTYPE_MASK, c"".as_ptr(), std::ptr::null_mut()) };
    if ours.is_null() {
        return f();
    }
    // SAFETY: `ours` is a valid locale object.
    let previous = unsafe { libc::uselocale(ours) };
    let _restore = Restore { previous, ours };
    f()
}

/// The command line as bytes, `argv[0]` first.
pub fn argv_bytes() -> Vec<Vec<u8>> {
    std::env::args_os().map(OsStringExt::into_vec).collect()
}

/// Port of main.c's option loop.
///
/// Arguments are processed left to right, as in jq: an error stops at the
/// first bad argument, and `-h`, `-V`, `--build-configuration` and
/// `--run-tests` take effect immediately (so `jq -h --bogus` prints help, and
/// `jq --bogus -h` fails). `--argjson`, `--jsonargs`, `--rawfile` and
/// `--slurpfile` are parsed or read right away, through `host`.
///
/// Arguments that aren't options: the first is the program (a file with
/// `-f`); later ones are input files, or `$ARGS.positional` strings/JSON after
/// `--args`/`--jsonargs` (the last of the two wins). Options may follow
/// `--args`, and the program may come after it. Everything after `--` is a
/// non-option.
pub fn parse<H: ArgHost>(argv: &[Vec<u8>], host: &mut H) -> Result<Action<H::Value>, ArgError> {
    let argc = argv.len();
    let mut o = Options::new(argv.first().cloned().unwrap_or_default());
    let mut further_args_are_strings = false;
    let mut further_args_are_json = false;
    let mut args_done = false;
    let mut i = 1;
    while i < argc {
        let arg = argv[i].as_slice();
        if args_done || !isoptish(arg) {
            if o.program.is_none() {
                o.program = Some(arg.to_vec());
            } else if further_args_are_strings {
                o.positional.push(ArgValue::Text(arg.to_vec()));
            } else if further_args_are_json {
                let v = host
                    .parse_json(arg)
                    .map_err(|_| ArgError::InvalidJsonargs)?;
                o.positional.push(ArgValue::Json(v));
            } else {
                o.files.push(arg.to_vec());
            }
            i += 1;
            continue;
        }
        if arg == b"--" {
            args_done = true;
            i += 1;
            continue;
        }
        // The first '-' was checked by isoptish.
        let is_short = arg[1] != b'-';
        let mut text: Option<&[u8]> = Some(if is_short { &arg[1..] } else { &arg[2..] });
        // One iteration for a long option; one per letter for short ones.
        while let Some(current) = text {
            let t = &mut text;
            if isoption(t, b's', "slurp", is_short) {
                o.slurp = true;
            } else if isoption(t, b'r', "raw-output", is_short) {
                o.raw_output = true;
            } else if isoption(t, 0, "raw-output0", is_short) {
                o.raw_output = true;
                o.raw_no_lf = true;
                o.raw_output0 = true;
            } else if isoption(t, b'j', "join-output", is_short) {
                o.raw_output = true;
                o.raw_no_lf = true;
            } else if isoption(t, b'c', "compact-output", is_short) {
                o.dumpopts &= !(TAB | indent_flags(7));
            } else if isoption(t, b'C', "color-output", is_short) {
                o.color_output = true;
            } else if isoption(t, b'M', "monochrome-output", is_short) {
                o.no_color_output = true;
            } else if isoption(t, b'a', "ascii-output", is_short) {
                o.ascii_output = true;
            } else if isoption(t, 0, "unbuffered", is_short) {
                o.unbuffered_output = true;
            } else if isoption(t, b'S', "sort-keys", is_short) {
                o.sorted_output = true;
            } else if isoption(t, b'R', "raw-input", is_short) {
                o.raw_input = true;
            } else if isoption(t, b'n', "null-input", is_short) {
                o.null_input = true;
            } else if isoption(t, b'f', "from-file", is_short) {
                o.from_file = true;
            } else if isoption(t, b'L', "library-path", is_short) {
                let paths = o.lib_search_paths.get_or_insert_with(Vec::new);
                if let Some(rest) = *t {
                    // -Ldir: the rest of the bundle is the directory.
                    paths.push(jq_realpath(rest));
                    *t = None;
                } else if i + 1 >= argc {
                    return Err(ArgError::LibraryPathMissing);
                } else {
                    paths.push(jq_realpath(&argv[i + 1]));
                    i += 1;
                }
            } else if isoption(t, b'b', "binary", is_short) {
                // Windows only (binary-mode stdio); accepted and ignored
                // elsewhere.
            } else if isoption(t, 0, "tab", is_short) {
                o.dumpopts &= !indent_flags(7);
                o.dumpopts |= TAB | PRETTY;
            } else if isoption(t, 0, "indent", is_short) {
                if i + 1 >= argc {
                    return Err(ArgError::IndentMissing);
                }
                let n = parse_indent(&argv[i + 1]).ok_or(ArgError::IndentInvalid)?;
                o.dumpopts &= !(TAB | indent_flags(7));
                o.dumpopts |= indent_flags(n);
                i += 1;
            } else if isoption(t, 0, "seq", is_short) {
                o.seq = true;
            } else if isoption(t, 0, "stream", is_short) {
                o.stream = true;
            } else if isoption(t, 0, "stream-errors", is_short) {
                o.stream = true;
                o.stream_errors = true;
            } else if isoption(t, b'e', "exit-status", is_short) {
                o.exit_status = true;
            } else if isoption(t, 0, "args", is_short) {
                further_args_are_strings = true;
                further_args_are_json = false;
            } else if isoption(t, 0, "jsonargs", is_short) {
                further_args_are_strings = false;
                further_args_are_json = true;
            } else if let Some(option) = named_option(t, is_short) {
                if i + 2 >= argc {
                    return Err(ArgError::MissingParameters(option));
                }
                let (name, param) = (&argv[i + 1], &argv[i + 2]);
                if !o.has_named(name) {
                    let value = named_value(option, name, param, host)?;
                    o.named.push((name.clone(), value));
                }
                i += 2;
            } else if isoption(t, 0, "debug-dump-disasm", is_short) {
                o.dump_disasm = true;
            } else if isoption(t, 0, "debug-trace=all", is_short) {
                o.jq_flags |= debug_flags::TRACE_ALL;
            } else if isoption(t, 0, "debug-trace", is_short) {
                o.jq_flags |= debug_flags::TRACE;
            } else if isoption(t, b'h', "help", is_short) {
                return Ok(Action::Help);
            } else if isoption(t, b'V', "version", is_short) {
                return Ok(Action::Version);
            } else if isoption(t, 0, "build-configuration", is_short) {
                return Ok(Action::BuildConfiguration);
            } else if isoption(t, 0, "run-tests", is_short) {
                let args = argv[i + 1..].to_vec();
                return Ok(Action::RunTests { options: o, args });
            }
            // qj extensions: only where jq would report an unknown option.
            else if isoption(t, 0, "threads", is_short) {
                if i + 1 >= argc {
                    return Err(ArgError::ThreadsMissing);
                }
                o.threads = Some(parse_threads(&argv[i + 1])?);
                i += 1;
            } else if let Some(n) = current.strip_prefix(b"threads=").filter(|_| !is_short) {
                o.threads = Some(parse_threads(n)?);
                *t = None;
            } else if isoption(t, 0, "jsonl", is_short) {
                o.jsonl = true;
            } else if isoption(t, 0, "debug-timing", is_short) {
                o.debug_timing = true;
            } else if is_short {
                return Err(ArgError::UnknownShortOption(current[0]));
            } else {
                return Err(ArgError::UnknownLongOption(current.to_vec()));
            }
        }
        i += 1;
    }
    Ok(Action::Run(o))
}

/// `--arg`, `--argjson`, `--rawfile` or `--slurpfile` (long options only).
fn named_option(text: &mut Option<&[u8]>, is_short: bool) -> Option<NamedOption> {
    [
        NamedOption::Arg,
        NamedOption::Argjson,
        NamedOption::Rawfile,
        NamedOption::Slurpfile,
    ]
    .into_iter()
    .find(|opt| isoption(text, 0, opt.name(), is_short))
}

/// The value of a named argument jq hasn't seen before.
fn named_value<H: ArgHost>(
    option: NamedOption,
    name: &[u8],
    param: &[u8],
    host: &mut H,
) -> Result<ArgValue<H::Value>, ArgError> {
    let bad_file = |message: Vec<u8>| ArgError::BadFile {
        option,
        name: name.to_vec(),
        file: param.to_vec(),
        message,
    };
    match option {
        NamedOption::Arg => Ok(ArgValue::Text(param.to_vec())),
        NamedOption::Argjson => host
            .parse_json(param)
            .map(ArgValue::Json)
            .map_err(|_| ArgError::InvalidArgjson),
        NamedOption::Rawfile => load_file(param).map(ArgValue::Text).map_err(bad_file),
        NamedOption::Slurpfile => {
            let data = load_file(param).map_err(bad_file)?;
            host.slurp_json(&data)
                .map(ArgValue::Json)
                .map_err(|m| bad_file(m.into_bytes()))
        }
    }
}

/// qj extension: expand glob patterns among input files.
///
/// An argument is replaced by its matches (sorted) only when it isn't `-`,
/// doesn't exist as a path, contains `*`, `?` or `[`, is a valid pattern and
/// matches something. Anything else is kept as is, so where jq would fail to
/// open a file, qj fails the same way.
pub fn expand_file_globs(files: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        match glob_matches(f) {
            Some(matches) => out.extend(matches),
            None => out.push(f.clone()),
        }
    }
    out
}

fn glob_matches(file: &[u8]) -> Option<Vec<Vec<u8>>> {
    if file == b"-" || !file.iter().any(|b| matches!(b, b'*' | b'?' | b'[')) {
        return None;
    }
    if std::fs::symlink_metadata(OsStr::from_bytes(file)).is_ok() {
        return None;
    }
    let pattern = std::str::from_utf8(file).ok()?;
    let mut matches: Vec<Vec<u8>> = glob::glob(pattern)
        .ok()?
        .filter_map(Result::ok)
        .map(|p| p.into_os_string().into_vec())
        .collect();
    if matches.is_empty() {
        return None;
    }
    matches.sort();
    Some(matches)
}

#[cfg(test)]
mod tests;
