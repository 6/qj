//! Port of jq 1.8.1's regex primitive `_match_impl/3` (`builtin.c: f_match`), which
//! `builtin.jq` builds `test`, `match`, `capture`, `scan`, `split/2`, `splits`, `sub` and
//! `gsub` on.
//!
//! The engine is Oniguruma 6.9.10 built from source by `onig_sys` (the FFI layer of the
//! `onig` crate). jq 1.8.1 vendors the same Oniguruma snapshot (the C sources are
//! identical), so this module calls it directly, the way `f_match` does: the same
//! syntax (`ONIG_SYNTAX_PERL_NG`), encoding (UTF-8), options, search calls on raw byte
//! pointers (jq's search can restart in the middle of a UTF-8 sequence), and error
//! text. The high-level `onig` API can't reproduce the error text exactly: it maps
//! non-UTF-8 messages (Oniguruma truncates long group names at 27 bytes, which can
//! split a character) to its own string.
//!
//! # Mapping to `_match_impl(re; modifiers; testmode)`
//!
//! The builtin layer checks the argument types in `f_match`'s order and raises these
//! type errors itself (`<kind>` is `jv_kind_name`, `<dump>` is
//! `jv_dump_string_trunc(v, buf, 15)`):
//!
//! 1. input not a string: `<kind> (<dump>) cannot be matched, as it is not a string`
//! 2. `re` not a string: `<kind> (<dump>) is not a string`
//! 3. `modifiers` neither a string nor null: `<kind> (<dump>) is not a string`
//!
//! Then it calls [`match_impl`] with `modifiers` as `None` for null. `testmode` is
//! test mode only if it equals `true` (`jv_equal(testmode, jv_true())`); any other
//! value, including `1` and `"true"`, means match mode.
//!
//! # Results as jq values
//!
//! In test mode the result is a boolean. In match mode it is an array of match
//! objects. jq prints object keys in insertion order, and `f_match` inserts them in
//! different orders on different paths, so the order is part of the result:
//!
//! - a match: [`Match::KEYS`] = `offset, length, string, captures`;
//! - a capture: [`Capture::keys`], which is `offset, length, string, name` for a
//!   non-empty capture of a non-empty match and `offset, string, length, name`
//!   otherwise.
//!
//! `offset` and `length` count codepoints (by jq's lead-byte rule, see
//! [`Match::offset`]); a capture group that did not participate has `offset` -1,
//! `length` 0 and `string` null.

use super::Error;
use super::utf8;
use onig_sys as onig;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_int, c_uint, c_void};
use std::ptr;
use std::rc::Rc;
use std::sync::{Mutex, Once};

/// Parse depth limit jq's `main()` sets with `onig_set_parse_depth_limit(1024)` (the
/// Oniguruma default is 4096), to avoid stack overflows (GHSA-f946-j5j2-4w5m). It
/// decides which deeply nested patterns fail with
/// `Regex failure: parse depth limit over`.
pub const PARSE_DEPTH_LIMIT: c_uint = 1024;

/// The parsed `modifiers` argument of `_match_impl`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags {
    /// `g`: find every match, not just the first.
    pub global: bool,
    /// The `OnigOptionType` bits `f_match` passes to `onig_new`.
    pub options: u32,
}

impl Flags {
    /// Port of `f_match`'s modifier loop. `None` stands for jq's `null`.
    ///
    /// | flag | effect |
    /// |---|---|
    /// | `g` | global search |
    /// | `i` | `ONIG_OPTION_IGNORECASE` |
    /// | `x` | `ONIG_OPTION_EXTEND` (extended syntax: whitespace and `#` comments) |
    /// | `m` | `ONIG_OPTION_MULTILINE` (Oniguruma's name for "`.` matches newline") |
    /// | `s` | `ONIG_OPTION_SINGLELINE` (`^` is `\A`, `$` is `\Z`) |
    /// | `p` | `MULTILINE` and `SINGLELINE` |
    /// | `l` | `ONIG_OPTION_FIND_LONGEST` |
    /// | `n` | `ONIG_OPTION_FIND_NOT_EMPTY` (empty matches are skipped) |
    ///
    /// `ONIG_OPTION_CAPTURE_GROUP` is always set, so unnamed groups capture even when the
    /// pattern has named groups. Any other character (including repeats of nothing, NUL,
    /// or non-ASCII) is an error naming the whole string, e.g.
    /// `gq is not a valid modifier string`.
    pub fn parse(modifiers: Option<&str>) -> Result<Flags, Error> {
        let mut flags = Flags {
            global: false,
            options: onig::ONIG_OPTION_CAPTURE_GROUP,
        };
        if let Some(mods) = modifiers {
            for c in mods.chars() {
                match c {
                    'g' => flags.global = true,
                    'i' => flags.options |= onig::ONIG_OPTION_IGNORECASE,
                    'x' => flags.options |= onig::ONIG_OPTION_EXTEND,
                    'm' => flags.options |= onig::ONIG_OPTION_MULTILINE,
                    's' => flags.options |= onig::ONIG_OPTION_SINGLELINE,
                    'p' => {
                        flags.options |= onig::ONIG_OPTION_MULTILINE | onig::ONIG_OPTION_SINGLELINE
                    }
                    'l' => flags.options |= onig::ONIG_OPTION_FIND_LONGEST,
                    'n' => flags.options |= onig::ONIG_OPTION_FIND_NOT_EMPTY,
                    _ => {
                        return Err(Error::msg(format!("{mods} is not a valid modifier string")));
                    }
                }
            }
        }
        Ok(flags)
    }
}

/// One match, as `f_match` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Codepoint offset of the match.
    ///
    /// jq counts codepoints by stepping from the start of the string by each lead byte's
    /// sequence length. After an empty match the next search starts one *byte* later,
    /// which can be inside a multi-byte character; a match found there reports the
    /// codepoint count up to the character containing it (so `"éé" | [match("";"g")]`
    /// has offsets 0, 1, 1, 2, 2).
    pub offset: usize,
    /// Length in codepoints (0 for an empty match).
    pub length: usize,
    /// The matched text. Invalid UTF-8 (possible when the match starts mid-character)
    /// becomes U+FFFD as in `jv_string_sized`.
    pub string: String,
    /// One entry per capture group, in group order.
    pub captures: Vec<Capture>,
}

impl Match {
    /// Key order of the match object.
    pub const KEYS: [&'static str; 4] = ["offset", "length", "string", "captures"];
}

/// One capture group of a [`Match`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    /// Codepoint offset, or -1 if the group did not participate in the match.
    ///
    /// Inside an empty match `f_match` reports every participating group at the match's
    /// own offset with an empty string, even a group that captured text in a lookahead:
    /// `"foo" | match("(?=(o+))")` has capture `{"offset":1,"string":"","length":0}`.
    pub offset: i64,
    /// Length in codepoints.
    pub length: usize,
    /// The captured text; `None` (jq `null`) if the group did not participate.
    pub string: Option<String>,
    /// The group's name, if it is a named group.
    pub name: Option<String>,
    /// The order `f_match` inserted this capture's keys in.
    pub key_order: CaptureKeyOrder,
}

/// Key insertion order of a capture object; see [`Capture::keys`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureKeyOrder {
    /// `offset, length, string, name`: a non-empty capture of a non-empty match.
    OffsetLengthStringName,
    /// `offset, string, length, name`: an empty or non-participating capture, and every
    /// capture of an empty match.
    OffsetStringLengthName,
}

impl Capture {
    /// The capture object's keys in jq's insertion (and output) order.
    pub fn keys(&self) -> [&'static str; 4] {
        match self.key_order {
            CaptureKeyOrder::OffsetLengthStringName => ["offset", "length", "string", "name"],
            CaptureKeyOrder::OffsetStringLengthName => ["offset", "string", "length", "name"],
        }
    }

    fn empty(offset: i64, participated: bool) -> Capture {
        Capture {
            offset,
            length: 0,
            string: participated.then(String::new),
            name: None,
            key_order: CaptureKeyOrder::OffsetStringLengthName,
        }
    }
}

/// Result of [`match_impl`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchResult {
    /// Test mode: whether the regex matched anywhere.
    Test(bool),
    /// Match mode: the matches (at most one unless the `g` flag was given).
    Matches(Vec<Match>),
}

/// Port of `f_match` for string arguments: `input | _match_impl(regex; modifiers; test)`.
///
/// Errors, in `f_match`'s order: invalid modifiers (see [`Flags::parse`]), then
/// `Regex failure: <Oniguruma message>` from compiling (e.g.
/// `Regex failure: end pattern with unmatched parenthesis`) or from searching (e.g.
/// `Regex failure: retry-limit-in-match over` on catastrophic backtracking).
pub fn match_impl(
    input: &str,
    regex: &str,
    modifiers: Option<&str>,
    test: bool,
) -> Result<MatchResult, Error> {
    let flags = Flags::parse(modifiers)?;
    let re = compile_cached(regex, flags.options)?;
    re.exec(input.as_bytes(), flags.global, test)
}

// ---------------------------------------------------------------------------------
// Oniguruma plumbing
// ---------------------------------------------------------------------------------

static INIT: Once = Once::new();

/// Serializes `onig_new`, like the `onig` crate does: Oniguruma's compiler touches
/// process-global tables and isn't documented as thread-safe.
static COMPILE_LOCK: Mutex<()> = Mutex::new(());

fn init() {
    INIT.call_once(|| {
        // SAFETY: plain calls into Oniguruma's global setup, done once before any
        // regex is compiled. jq's main() sets the depth limit first; the first
        // onig_new would otherwise initialize Oniguruma with the UTF-8 encoding
        // implicitly, which is what onig_initialize does here.
        unsafe {
            onig::onig_set_parse_depth_limit(PARSE_DEPTH_LIMIT);
            let mut encodings: [onig::OnigEncoding; 1] = [&raw mut onig::OnigEncodingUTF8];
            onig::onig_initialize(encodings.as_mut_ptr(), 1);
        }
    });
}

/// A compiled regex plus its group names (from `onig_foreach_name`).
struct Compiled {
    raw: onig::OnigRegex,
    /// `(name, group numbers)`; a name can label several groups.
    names: Vec<(String, Vec<c_int>)>,
}

impl Drop for Compiled {
    fn drop(&mut self) {
        // SAFETY: `raw` came from a successful onig_new and is freed exactly once.
        unsafe { onig::onig_free(self.raw) }
    }
}

/// `onig_error_code_to_str(ebuf, code, einfo)` as a jq string: `jv_string((char*)ebuf)`
/// reads up to the first NUL (a NUL inside a group name truncates the message) and
/// replaces invalid UTF-8.
fn error_text(code: c_int, einfo: &onig::OnigErrorInfo) -> String {
    let mut buf = [0u8; onig::ONIG_MAX_ERROR_MESSAGE_LEN as usize];
    // SAFETY: the buffer has ONIG_MAX_ERROR_MESSAGE_LEN bytes, as Oniguruma requires;
    // einfo's pointers (if used by this code) point into the still-live pattern.
    unsafe {
        onig::onig_error_code_to_str(buf.as_mut_ptr(), code, einfo as *const onig::OnigErrorInfo);
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    utf8::string_sized(&buf[..len])
}

fn regex_failure(code: c_int, einfo: &onig::OnigErrorInfo) -> Error {
    Error::Msg(format!("Regex failure: {}", error_text(code, einfo)))
}

unsafe extern "C" fn collect_name(
    name: *const onig::OnigUChar,
    name_end: *const onig::OnigUChar,
    ngroups: c_int,
    groups: *mut c_int,
    _reg: onig::OnigRegex,
    arg: *mut c_void,
) -> c_int {
    // SAFETY: Oniguruma passes a valid name range and group array; `arg` is the
    // `Vec` that `compile` handed to onig_foreach_name.
    unsafe {
        let names = &mut *(arg as *mut Vec<(String, Vec<c_int>)>);
        let len = name_end.offset_from(name) as usize;
        let bytes = std::slice::from_raw_parts(name, len);
        let groups = std::slice::from_raw_parts(groups, ngroups.max(0) as usize).to_vec();
        names.push((utf8::string_sized(bytes), groups));
    }
    0
}

fn compile(pattern: &str, options: u32) -> Result<Compiled, Error> {
    init();
    let bytes = pattern.as_bytes();
    let mut raw: onig::OnigRegex = ptr::null_mut();
    let mut einfo = onig::OnigErrorInfo {
        enc: ptr::null_mut(),
        par: ptr::null_mut(),
        par_end: ptr::null_mut(),
    };
    let ret = {
        let _guard = COMPILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the pattern range is a live byte slice; the encoding and syntax are
        // Oniguruma's own statics, which onig_new only reads.
        unsafe {
            onig::onig_new(
                &mut raw,
                bytes.as_ptr(),
                bytes.as_ptr().add(bytes.len()),
                options,
                &raw mut onig::OnigEncodingUTF8,
                &raw mut onig::OnigSyntaxPerl_NG,
                &mut einfo,
            )
        }
    };
    if ret != onig::ONIG_NORMAL as c_int {
        // onig_new has already freed the regex and nulled `raw`.
        return Err(regex_failure(ret, &einfo));
    }
    let mut compiled = Compiled {
        raw,
        names: Vec::new(),
    };
    // SAFETY: `raw` is a valid regex; the callback only touches `names`.
    unsafe {
        onig::onig_foreach_name(
            raw,
            Some(collect_name),
            &mut compiled.names as *mut Vec<(String, Vec<c_int>)> as *mut c_void,
        );
    }
    Ok(compiled)
}

/// Compiled regexes are cached per thread. jq recompiles on every call; compiling is
/// deterministic, so reusing a compiled regex is not observable.
const CACHE_LIMIT: usize = 256;

thread_local! {
    /// options -> pattern -> regex (two levels so a hit doesn't allocate a key).
    static CACHE: RefCell<HashMap<u32, HashMap<String, Rc<Compiled>>>> =
        RefCell::new(HashMap::new());
}

fn compile_cached(pattern: &str, options: u32) -> Result<Rc<Compiled>, Error> {
    let hit = CACHE.with(|c| {
        c.borrow()
            .get(&options)
            .and_then(|by_pattern| by_pattern.get(pattern))
            .cloned()
    });
    if let Some(re) = hit {
        return Ok(re);
    }
    let re = Rc::new(compile(pattern, options)?);
    CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if cache.values().map(HashMap::len).sum::<usize>() >= CACHE_LIMIT {
            cache.clear();
        }
        cache
            .entry(options)
            .or_default()
            .insert(pattern.to_owned(), re.clone());
    });
    Ok(re)
}

/// An `OnigRegion`, freed on drop.
struct Region(*mut onig::OnigRegion);

impl Region {
    fn new() -> Region {
        // SAFETY: allocation only.
        let raw = unsafe { onig::onig_region_new() };
        assert!(!raw.is_null(), "onig_region_new: out of memory");
        Region(raw)
    }

    fn num_regs(&self) -> usize {
        // SAFETY: valid region.
        unsafe { (*self.0).num_regs.max(0) as usize }
    }

    /// `(region->beg[i], region->end[i])`: byte offsets, -1 if the group didn't match.
    fn span(&self, i: usize) -> (c_int, c_int) {
        // SAFETY: `i < num_regs`, and onig_search sized both arrays to num_regs.
        unsafe {
            let r = &*self.0;
            (*r.beg.add(i), *r.end.add(i))
        }
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        // SAFETY: frees the arrays and the struct, like `onig_region_free(region, 1)`.
        unsafe { onig::onig_region_free(self.0, 1) }
    }
}

/// `for (idx = 0; fr < input+pos; idx++) fr += jvp_utf8_decode_length(*fr);`
fn codepoints_before(s: &[u8], pos: usize) -> usize {
    let (mut fr, mut idx) = (0, 0);
    while fr < pos {
        fr += utf8::decode_length(s[fr]);
        idx += 1;
    }
    idx
}

/// `f_match`'s offset/length loop for a non-empty range:
/// `for (idx = len = 0; fr < input+end; len++) { if (fr == input+beg) idx = len, len = 0; fr += ...; }`
///
/// If `beg` falls inside a character, `idx` stays 0 and `len` counts every character
/// before `end`, exactly like jq.
fn offset_and_length(s: &[u8], beg: usize, end: usize) -> (usize, usize) {
    let (mut fr, mut idx, mut len) = (0, 0, 0);
    while fr < end {
        if fr == beg {
            idx = len;
            len = 0;
        }
        fr += utf8::decode_length(s[fr]);
        len += 1;
    }
    (idx, len)
}

/// `jv_string_sized(input + beg, end - beg)`.
fn slice_string(s: &[u8], beg: usize, end: usize) -> String {
    // A group can't end before it begins in practice; don't panic if it ever does.
    utf8::string_sized(s.get(beg..end.max(beg)).unwrap_or_default())
}

impl Compiled {
    /// The search loop of `f_match`.
    fn exec(&self, s: &[u8], global: bool, test: bool) -> Result<MatchResult, Error> {
        let region = Region::new();
        let base = s.as_ptr();
        let len = s.len();
        let mut matches = Vec::new();
        let mut start = 0usize;
        loop {
            // SAFETY: `start <= len` (the loop condition below), so every pointer is
            // within or one past the end of `s`. The search may start inside a
            // multi-byte character, as in jq.
            let ret = unsafe {
                onig::onig_search(
                    self.raw,
                    base,
                    base.add(len),
                    base.add(start),
                    base.add(len),
                    region.0,
                    onig::ONIG_OPTION_NONE,
                )
            };
            if ret >= 0 {
                if test {
                    return Ok(MatchResult::Test(true));
                }
                let (beg0, end0) = region.span(0);
                let (beg0, end0) = (beg0.max(0) as usize, end0.max(0) as usize);
                if beg0 == end0 {
                    // Zero-width match.
                    let idx = codepoints_before(s, beg0);
                    let mut captures: Vec<Capture> = (1..region.num_regs())
                        .map(|i| {
                            if region.span(i).0 == -1 {
                                Capture::empty(-1, false)
                            } else {
                                Capture::empty(idx as i64, true)
                            }
                        })
                        .collect();
                    self.set_names(&mut captures);
                    matches.push(Match {
                        offset: idx,
                        length: 0,
                        string: String::new(),
                        captures,
                    });
                    // "ensure '"qux" | match("(?=u)"; "g")' matches just once": skip one
                    // byte, not one character.
                    start = end0 + 1;
                } else {
                    let (idx, mlen) = offset_and_length(s, beg0, end0);
                    let string = slice_string(s, beg0, end0);
                    let mut captures = Vec::with_capacity(region.num_regs().saturating_sub(1));
                    for i in 1..region.num_regs() {
                        let (b, e) = region.span(i);
                        if b == e {
                            captures.push(if b == -1 {
                                Capture::empty(-1, false)
                            } else {
                                Capture::empty(codepoints_before(s, b as usize) as i64, true)
                            });
                            continue;
                        }
                        let (b, e) = (b.max(0) as usize, e.max(0) as usize);
                        let (cidx, clen) = offset_and_length(s, b, e);
                        captures.push(Capture {
                            offset: cidx as i64,
                            length: clen,
                            string: Some(slice_string(s, b, e)),
                            name: None,
                            key_order: CaptureKeyOrder::OffsetLengthStringName,
                        });
                    }
                    self.set_names(&mut captures);
                    matches.push(Match {
                        offset: idx,
                        length: mlen,
                        string,
                        captures,
                    });
                    start = end0;
                }
            } else if ret == onig::ONIG_MISMATCH {
                break;
            } else {
                let einfo = onig::OnigErrorInfo {
                    enc: &raw mut onig::OnigEncodingUTF8,
                    par: ptr::null_mut(),
                    par_end: ptr::null_mut(),
                };
                return Err(regex_failure(ret, &einfo));
            }
            if !(global && start <= len) {
                break;
            }
        }
        Ok(if test {
            MatchResult::Test(false)
        } else {
            MatchResult::Matches(matches)
        })
    }

    /// `onig_foreach_name(reg, f_match_name_iter, &captures)`.
    fn set_names(&self, captures: &mut [Capture]) {
        for (name, groups) in &self.names {
            for &g in groups {
                if let Some(cap) = usize::try_from(g - 1)
                    .ok()
                    .and_then(|i| captures.get_mut(i))
                {
                    cap.name = Some(name.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
