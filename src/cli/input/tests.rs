//! Tests for the util.c reader. Expectations come from running jq 1.8.1 on
//! the same inputs (the commands are in the comments).

use std::collections::HashMap;
use std::io::{self, Read};

use super::*;
use crate::jq::value::DumpOptions;
use crate::jq::value::print::dump_to_vec;

/// A reader that returns at most `step` bytes per read, then optionally fails.
struct Trickle {
    data: Vec<u8>,
    pos: usize,
    step: usize,
    fail_at_end: Option<i32>,
}

impl Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.data.len()
            && let Some(errno) = self.fail_at_end
        {
            return Err(io::Error::from_raw_os_error(errno));
        }
        let n = buf.len().min(self.step).min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn trickle(data: &[u8], step: usize) -> Box<dyn Read> {
    Box::new(Trickle {
        data: data.to_vec(),
        pos: 0,
        step,
        fail_at_end: None,
    })
}

/// fgets chunks of a stream, as (chunk, `strlen` view) pairs.
fn chunks(data: &[u8], step: usize) -> Vec<Vec<u8>> {
    let mut s = Stream::new(trickle(data, step));
    let mut out = Vec::new();
    let mut buf = Vec::new();
    while s.fgets(&mut buf) {
        out.push(buf.clone());
    }
    out
}

#[test]
fn fgets_reads_lines_and_4095_byte_pieces() {
    for step in [1, 7, 4096, 1 << 20] {
        assert_eq!(chunks(b"abc\ndef", step), [&b"abc\n"[..], b"def"]);
        assert_eq!(chunks(b"\n\n", step), [b"\n", b"\n"]);
        assert!(chunks(b"", step).is_empty());
        let mut long = vec![b'a'; 5000];
        long.push(b'\n');
        let c = chunks(&long, step);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].len(), 4095);
        assert_eq!(c[1].len(), 906);
        // Exactly 4095 bytes and then a newline: the newline is a chunk.
        let mut exact = vec![b'b'; 4095];
        exact.push(b'\n');
        assert_eq!(chunks(&exact, step).len(), 2);
        // NULs don't end a chunk.
        assert_eq!(chunks(b"a\0b\nc\0d", step), [&b"a\0b\n"[..], b"c\0d"]);
    }
}

#[test]
fn fgets_loses_a_partial_line_on_a_read_error() {
    let mut s = Stream::new(Box::new(Trickle {
        data: b"1\n2".to_vec(),
        pos: 0,
        step: 1,
        fail_at_end: Some(libc::EIO),
    }));
    let mut buf = Vec::new();
    assert!(s.fgets(&mut buf));
    assert_eq!(buf, b"1\n");
    assert!(!s.fgets(&mut buf));
    assert!(buf.is_empty());
    assert_eq!(
        s.error.as_ref().and_then(io::Error::raw_os_error),
        Some(libc::EIO)
    );
}

/// A reader over named in-memory inputs; unknown names fail with ENOENT.
/// Messages are collected as jq would print them.
fn reader(
    files: &[&str],
    contents: &[(&str, &[u8])],
    opts: InputOptions,
) -> (UtilInput, std::rc::Rc<std::cell::RefCell<Vec<u8>>>) {
    let map: HashMap<Vec<u8>, Vec<u8>> = contents
        .iter()
        .map(|(n, c)| (n.as_bytes().to_vec(), c.to_vec()))
        .collect();
    let files = files.iter().map(|f| f.as_bytes().to_vec()).collect();
    let mut r = UtilInput::with_opener(
        files,
        opts,
        Box::new(move |name: &[u8]| match map.get(name) {
            Some(c) => Ok(trickle(c, 3)),
            None => Err(io::Error::from_raw_os_error(libc::ENOENT)),
        }),
    );
    let messages = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let m = messages.clone();
    r.set_message_sink(Box::new(move |msg| {
        m.borrow_mut().extend_from_slice(&msg.render("jq"))
    }));
    (r, messages)
}

/// Every result as compact JSON, or `error: ...`, with the position and
/// failure count after each.
fn drain(r: &mut UtilInput) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(v) = r.next() {
        let text = match v {
            Ok(v) => {
                let mut b = Vec::new();
                dump_to_vec(&v, &DumpOptions::compact(), &mut b);
                String::from_utf8(b).unwrap()
            }
            Err(e) => format!("error: {e}"),
        };
        out.push(format!("{text} @{} f{}", r.position(), r.failures()));
    }
    out
}

fn json() -> InputOptions {
    InputOptions::default()
}

#[test]
fn texts_continue_across_files() {
    // printf 1 > a1; printf 2 > a2; jq -c . a1 a2
    let (mut r, _) = reader(&["a1", "a2"], &[("a1", b"1"), ("a2", b"2")], json());
    assert_eq!(drain(&mut r), ["12 @a2:0 f0"]);
    // printf '[1,\n2' > a3; printf ',3]\n' > a4; jq -c . a3 a4
    let (mut r, _) = reader(
        &["a3", "a4"],
        &[("a3", b"[1,\n2"), ("a4", b",3]\n")],
        json(),
    );
    assert_eq!(drain(&mut r), ["[1,2,3] @a4:1 f0"]);
}

#[test]
fn line_numbers_and_file_names() {
    // jq -c '[., input_filename, input_line_number]' lines3.json a1 a2
    let (mut r, _) = reader(
        &["lines3.json", "a1", "a2"],
        &[("lines3.json", b"1\n2\n3\n"), ("a1", b"1"), ("a2", b"2")],
        json(),
    );
    assert_eq!(r.position(), "<unknown>");
    assert!(matches!(r.current_filename(), Value::Null));
    assert_eq!(
        drain(&mut r),
        [
            "1 @lines3.json:1 f0",
            "2 @lines3.json:2 f0",
            "3 @lines3.json:3 f0",
            "12 @a2:0 f0"
        ]
    );
}

#[test]
fn a_missing_file_is_reported_and_counted() {
    // jq -c . a3 nonexist a4: the message, then [1,2,3], exit 2.
    let (mut r, msgs) = reader(
        &["a3", "nonexist", "a4"],
        &[("a3", b"[1,\n2"), ("a4", b",3]\n")],
        json(),
    );
    assert_eq!(drain(&mut r), ["[1,2,3] @a4:1 f1"]);
    assert_eq!(
        String::from_utf8(msgs.borrow().clone()).unwrap(),
        "jq: error: Could not open file nonexist: No such file or directory\n"
    );
}

#[test]
fn a_read_error_is_reported_when_the_next_input_opens() {
    // jq . /tmp f.json: "jq: error: Is a directory", then 1 (and exit 2).
    let files = vec![b"dir".to_vec(), b"f".to_vec()];
    let mut r = UtilInput::with_opener(
        files,
        json(),
        Box::new(|name: &[u8]| -> io::Result<Box<dyn Read>> {
            Ok(Box::new(Trickle {
                data: if name == b"f" {
                    b"1\n2".to_vec()
                } else {
                    Vec::new()
                },
                pos: 0,
                step: 4096,
                fail_at_end: (name == b"dir").then_some(libc::EISDIR),
            }))
        }),
    );
    let msgs = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let m = msgs.clone();
    r.set_message_sink(Box::new(move |msg| {
        m.borrow_mut().extend_from_slice(&msg.render("jq"))
    }));
    assert_eq!(drain(&mut r)[0], "1 @f:1 f1");
    assert_eq!(
        String::from_utf8(msgs.borrow().clone()).unwrap(),
        "jq: error: Is a directory\n"
    );
}

#[test]
fn chunks_without_a_newline_stop_at_nul() {
    // printf '1 2\0003 4' | jq -c .   => 1, 2
    let (mut r, _) = reader(&["-"], &[("-", b"1 2\x003 4")], json());
    assert_eq!(drain(&mut r), ["1 @<stdin>:0 f0", "2 @<stdin>:0 f0"]);
}

#[test]
fn raw_lines_join_across_files() {
    let raw = InputOptions {
        raw: true,
        ..InputOptions::default()
    };
    let files: &[(&str, &[u8])] = &[
        ("a1", b"1"),
        ("a2", b"2"),
        ("a3", b"[1,\n2"),
        ("a4", b",3]\n"),
    ];
    // jq -R -c . a1 a2 a3 a4
    let (mut r, _) = reader(&["a1", "a2", "a3", "a4"], files, raw);
    assert_eq!(drain(&mut r), [r#""12[1," @a3:1 f0"#, r#""2,3]" @a4:1 f0"#]);
    // jq -Rs -c . a1 a2 a3 a4
    let raw_slurp = InputOptions {
        raw: true,
        slurp: true,
        ..InputOptions::default()
    };
    let (mut r, _) = reader(&["a1", "a2", "a3", "a4"], files, raw_slurp);
    assert_eq!(drain(&mut r), [r#""12[1,\n2,3]\n" @a4:1 f0"#]);
}

#[test]
fn raw_chunks_are_repaired_one_by_one() {
    // A euro sign split by the 4095-byte chunk boundary: jq -R 'length' gives
    // 4097 (its three bytes become three U+FFFD).
    let raw = InputOptions {
        raw: true,
        ..InputOptions::default()
    };
    let mut line = vec![b'a'; 4094];
    line.extend_from_slice("€\n".as_bytes());
    let (mut r, _) = reader(&["-"], &[("-", &line)], raw);
    match r.next() {
        Some(Ok(Value::String(s))) => {
            assert_eq!(s.codepoint_len(), 4097);
            assert!(s.as_str().ends_with("\u{FFFD}\u{FFFD}\u{FFFD}"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn slurp_returns_parse_errors_then_the_array() {
    let slurp = InputOptions {
        slurp: true,
        flags: ParseFlags {
            seq: true,
            ..ParseFlags::default()
        },
        ..InputOptions::default()
    };
    // printf '\x1e1\n\x1e[2}\n\x1e3\n' | jq --seq -s -c '[., input_line_number]'
    let (mut r, _) = reader(&["-"], &[("-", b"\x1e1\n\x1e[2}\n\x1e3\n")], slurp);
    assert_eq!(
        drain(&mut r),
        [
            "error: Objects must consist of key:value pairs at line 2, column 4 \
             (need RS to resync) @<stdin>:2 f0",
            "[1,3] @<stdin>:3 f0"
        ]
    );
}

#[test]
fn stdin_is_opened_once() {
    // jq -c . - - reads standard input once; the second `-` is at EOF.
    let opened = std::rc::Rc::new(std::cell::Cell::new(0));
    let o = opened.clone();
    let mut r = UtilInput::with_opener(
        vec![b"-".to_vec(), b"-".to_vec()],
        json(),
        Box::new(move |_: &[u8]| -> io::Result<Box<dyn Read>> {
            o.set(o.get() + 1);
            Ok(trickle(b"1 2\n", 2))
        }),
    );
    assert_eq!(drain(&mut r), ["1 @<stdin>:1 f0", "2 @<stdin>:1 f0"]);
    assert_eq!(opened.get(), 1);
}
