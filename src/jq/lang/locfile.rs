//! Port of jq 1.8.1's `locfile.c`: source text plus a line map, used to turn byte
//! locations into `line N, column M` and to format compile errors with the offending
//! source line and a caret underline.
//!
//! ```text
//! jq: error: syntax error, unexpected IDENT, expecting end of file at <top-level>, line 1, column 4:
//!     .a b
//!        ^
//! ```
//!
//! Both the parser (syntax errors) and the compiler (e.g. `$x is not defined`) report
//! through [`LocFile::locate`].

use super::ast::Loc;
use super::lexer::jv_string_sized;

/// `struct locfile`: a named source text with its line map.
#[derive(Clone, Debug)]
pub struct LocFile {
    fname: String,
    data: Vec<u8>,
    /// `linemap[i]` is the byte offset where line `i` starts; `linemap[nlines]` is
    /// `length + 1` (a virtual final newline).
    linemap: Vec<u32>,
}

impl LocFile {
    /// `locfile_init(jq, fname, data, length)`. `fname` is `<top-level>` for the main
    /// program, `<builtin>` for builtin.jq, or a module's path.
    pub fn new(fname: &str, data: &[u8]) -> LocFile {
        let mut linemap = Vec::with_capacity(data.len() / 32 + 2);
        linemap.push(0);
        for (i, &b) in data.iter().enumerate() {
            if b == b'\n' {
                linemap.push(i as u32 + 1); // at start of line, not of \n
            }
        }
        linemap.push(data.len() as u32 + 1); // virtual last \n
        LocFile {
            fname: jv_string_sized(fname.as_bytes()).into_owned(),
            data: data.to_vec(),
            linemap,
        }
    }

    pub fn fname(&self) -> &str {
        &self.fname
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Number of lines (`nlines`): one more than the number of `\n` bytes.
    pub fn nlines(&self) -> usize {
        self.linemap.len() - 1
    }

    /// `locfile_get_line`: the 0-based line containing byte `pos`. jq asserts
    /// `pos < length`; out-of-range positions map to the last line here.
    pub fn get_line(&self, pos: u32) -> usize {
        // Largest line whose start is <= pos (jq scans linearly; same result).
        let n = self.linemap[1..self.nlines()].partition_point(|&start| start <= pos);
        n.min(self.nlines() - 1)
    }

    /// `locfile_line_length`: length of `line` in bytes, without its `\n`.
    fn line_length(&self, line: usize) -> usize {
        (self.linemap[line + 1] - self.linemap[line] - 1) as usize
    }

    /// `locfile_locate(l, loc, "%s", msg)`: the text jq hands to `jq_report_error`
    /// (printed by the default error callback followed by a newline).
    ///
    /// `msg` is the complete first part, e.g. `"jq: error: syntax error, ..."`. For
    /// [`Loc::UNKNOWN`] jq prefixes `"jq: error: "` once more, verbatim.
    pub fn locate(&self, loc: Loc, msg: &str) -> String {
        if loc.is_unknown() {
            return format!("jq: error: {msg}");
        }
        let start = loc.start as i64;
        let startline = self.get_line(loc.start);
        let offset = self.linemap[startline] as i64;
        let eol = self.linemap[startline + 1] as i64 - 1;
        let end = (loc.end as i64).min(eol.max(start + 1));
        let underline = "^".repeat((end - start).max(0) as usize);

        let mut out: Vec<u8> = Vec::with_capacity(msg.len() + 64 + self.line_length(startline));
        out.extend_from_slice(msg.as_bytes());
        out.extend_from_slice(b" at ");
        out.extend_from_slice(self.fname.as_bytes());
        out.extend_from_slice(
            format!(
                ", line {}, column {}:\n    ",
                startline + 1,
                start - offset + 1
            )
            .as_bytes(),
        );
        let line_start = offset as usize;
        let line_end = (line_start + self.line_length(startline)).min(self.data.len());
        out.extend_from_slice(&self.data[line_start.min(line_end)..line_end]);
        out.extend_from_slice(b"\n    ");
        // printf("%*s", end - offset, underline): right-align in a field of that width
        // (a negative width left-aligns).
        let width = end - offset;
        let pad = width.unsigned_abs() as usize;
        if width >= 0 {
            out.extend(std::iter::repeat_n(
                b' ',
                pad.saturating_sub(underline.len()),
            ));
            out.extend_from_slice(underline.as_bytes());
        } else {
            out.extend_from_slice(underline.as_bytes());
            out.extend(std::iter::repeat_n(
                b' ',
                pad.saturating_sub(underline.len()),
            ));
        }
        jv_string_sized(&out).into_owned()
    }
}

/// The summary line `jq_compile_args` prints after compile errors.
pub fn compile_errors_summary(nerrors: usize) -> String {
    format!(
        "jq: {} compile {}",
        nerrors,
        if nerrors > 1 { "errors" } else { "error" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_lookup() {
        let lf = LocFile::new("<top-level>", b"ab\ncd\n\nef");
        assert_eq!(lf.nlines(), 4);
        assert_eq!(lf.get_line(0), 0);
        assert_eq!(lf.get_line(2), 0); // the '\n' belongs to its line
        assert_eq!(lf.get_line(3), 1);
        assert_eq!(lf.get_line(6), 2);
        assert_eq!(lf.get_line(7), 3);
        assert_eq!(lf.get_line(9), 3);
    }

    #[test]
    fn locate_formats_like_jq() {
        let lf = LocFile::new("<top-level>", b".a b");
        assert_eq!(
            lf.locate(Loc::new(3, 4), "jq: error: boom"),
            "jq: error: boom at <top-level>, line 1, column 4:\n    .a b\n       ^"
        );
        // The underline stops at the end of the start line.
        let lf = LocFile::new("<top-level>", b"if . then 1\nelse 2");
        assert_eq!(
            lf.locate(Loc::new(0, 18), "m"),
            "m at <top-level>, line 1, column 1:\n    if . then 1\n    ^^^^^^^^^^^"
        );
        // Empty span: no carets, only padding.
        assert_eq!(
            lf.locate(Loc::new(3, 3), "m"),
            "m at <top-level>, line 1, column 4:\n    if . then 1\n       "
        );
        assert_eq!(lf.locate(Loc::UNKNOWN, "x"), "jq: error: x");
    }

    #[test]
    fn summary() {
        assert_eq!(compile_errors_summary(1), "jq: 1 compile error");
        assert_eq!(compile_errors_summary(2), "jq: 2 compile errors");
    }
}
