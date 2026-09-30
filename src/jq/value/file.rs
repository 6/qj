//! Port of jq's `jv_file.c`: `jv_load_file`, used by `--slurpfile`,
//! `--rawfile` and module data imports.

use std::io::Read;
use std::path::Path;

use super::unicode::utf8_backtrack;
use super::{Array, Error, ParseFlags, Parser, Str, Value};

/// `strerror(errno)` text, as jq prints it (not Rust's `io::Error` format;
/// see [`crate::jq::platform::strerror`]). `jv_string_fmt` repairs invalid
/// UTF-8, as the lossy conversion does.
fn strerror(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => String::from_utf8_lossy(&crate::jq::platform::strerror(code)).into_owned(),
        None => e.to_string(),
    }
}

/// `jv_load_file(filename, raw)`: with `raw`, the file as one string
/// (invalid UTF-8 repaired); otherwise every JSON value in the file, as an
/// array. Errors: `Could not open <f>: <strerror>`, `Could not open <f>:
/// It's a directory`, `Error reading from <f>`, or the first parse error.
///
/// Like jq, the file is read in 4096-byte chunks (extended to finish a
/// UTF-8 sequence) and each chunk is marked partial until a read hits end of
/// file; so a file whose size is a multiple of 4096 never gets a final
/// non-partial chunk, and a trailing top-level scalar without whitespace
/// after it is dropped, exactly as jq does.
pub fn load_file(filename: &str, raw: bool) -> Result<Value, Error> {
    let path = Path::new(filename);
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return Err(Error::msg(format!(
                "Could not open {filename}: {}",
                strerror(&e)
            )));
        }
    };
    match file.metadata() {
        Ok(m) if !m.is_dir() => {}
        _ => {
            return Err(Error::msg(format!(
                "Could not open {filename}: It's a directory"
            )));
        }
    }
    let mut data_str = Str::new();
    let mut data_arr = Array::new();
    let mut parser = if raw {
        None
    } else {
        Some(Parser::new(ParseFlags::default()))
    };
    const CHUNK: usize = 4096;
    const MAX_UTF8_LEN: usize = 4;
    let mut buf = vec![0u8; CHUNK + MAX_UTF8_LEN];
    let mut eof = false;
    let read_err = |_| Error::msg(format!("Error reading from {filename}"));
    while !eof {
        let (n, hit_eof) = read_full(&mut file, &mut buf[..CHUNK]).map_err(read_err)?;
        eof = hit_eof;
        let mut n = n;
        if n == 0 {
            continue;
        }
        let mut missing = 0usize;
        if utf8_backtrack(&buf, n - 1, 0, Some(&mut missing)).is_some() && missing > 0 && !eof {
            let (m, hit_eof) = read_full(&mut file, &mut buf[n..n + missing]).map_err(read_err)?;
            n += m;
            eof = hit_eof;
        }
        match &mut parser {
            None => data_str.push_bytes(&buf[..n]),
            Some(p) => {
                p.set_buf(&buf[..n], !eof);
                loop {
                    match p.next() {
                        Some(Ok(v)) => data_arr.push(v),
                        Some(Err(e)) => return Err(e),
                        None => break,
                    }
                }
            }
        }
    }
    Ok(if raw {
        Value::String(data_str)
    } else {
        Value::Array(data_arr)
    })
}

/// `fread`: reads until `buf` is full or end of file. Returns the byte count
/// and whether end of file was hit (a short read).
fn read_full(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<(usize, bool)> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..]) {
            Ok(0) => return Ok((n, true)),
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok((n, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_file_errors_and_values() {
        let dir = std::env::temp_dir();
        let missing = dir.join("qj-value-load-file-missing.json");
        let m = missing.to_str().unwrap();
        // `jq --slurpfile x /nonexistent .` reports strerror text.
        assert_eq!(
            load_file(m, false).unwrap_err().to_string(),
            format!("Could not open {m}: No such file or directory")
        );
        let d = dir.to_str().unwrap();
        assert_eq!(
            load_file(d, true).unwrap_err().to_string(),
            format!("Could not open {d}: It's a directory")
        );
        let f = dir.join(format!("qj-value-load-file-{}.json", std::process::id()));
        std::fs::write(&f, b"1 [2] {\"a\":3}\n\"\xe2a\"").unwrap();
        let fs = f.to_str().unwrap();
        assert_eq!(
            load_file(fs, false).unwrap().to_json(),
            "[1,[2],{\"a\":3},\"\u{FFFD}\"]"
        );
        // Raw: E2 is followed by `a"`, so only E2 is replaced (inside the
        // JSON string token above, E2 `a` ran into the token end instead).
        assert_eq!(
            load_file(fs, true).unwrap().to_json(),
            "\"1 [2] {\\\"a\\\":3}\\n\\\"\u{FFFD}a\\\"\""
        );
        std::fs::write(&f, b"[1,2").unwrap();
        assert_eq!(
            load_file(fs, false).unwrap_err().to_string(),
            "Unfinished JSON term at EOF at line 1, column 4"
        );
        std::fs::remove_file(&f).unwrap();
    }
}
