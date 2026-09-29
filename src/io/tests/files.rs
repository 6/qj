//! Inputs from the file system through the default opener: memory-mapped
//! files, standard input redirected from a file, and compressed files (a
//! qj extension: they read exactly like their decompressed content).

use std::ffi::OsString;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;

use super::generate::Gen;
use super::{Delivery, Ev, MemFile, Rng, events, mem_reader};
use crate::io::parallel::{self, DumpFactory, EngineOptions, RecordSink};
use crate::io::reader::{InputReader, ReaderOptions};
use crate::io::source::{Opened, open_borrowed_fd};
use crate::jq::value::{DumpOptions, Error};

fn sample_ndjson(seed: u64, records: usize) -> Vec<u8> {
    let mut r = Rng(seed);
    let mut g = Gen {
        r: &mut r,
        weird: 0,
    };
    let mut out = Vec::new();
    for i in 0..records {
        g.value(&mut out, 0, false);
        if i % 97 == 50 {
            out.extend_from_slice(b"\n[1,\n2]"); // a text spanning lines
        }
        out.push(b'\n');
    }
    out
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// Events for the same inputs read from memory (the expectation).
fn from_memory(names: &[&str], contents: &[Vec<u8>], opts: ReaderOptions) -> Vec<Ev> {
    let files: Vec<(OsString, MemFile)> = names
        .iter()
        .zip(contents)
        .map(|(n, c)| (OsString::from(n), MemFile::Data(c.clone())))
        .collect();
    let (mut r, msgs) = mem_reader(names, files, opts, Delivery::Whole, true);
    events(&mut r, &msgs, 100_000)
}

/// Events from the real files, reported under the given names.
fn from_disk(dir: &std::path::Path, names: &[&str], opts: ReaderOptions) -> Vec<Ev> {
    let paths: Vec<OsString> = names.iter().map(|n| dir.join(n).into_os_string()).collect();
    let mut r = InputReader::new(paths, opts);
    let msgs = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = msgs.clone();
    r.set_message_sink(Box::new(move |m| sink.borrow_mut().extend(m.render("jq"))));
    let prefix = format!("{}/", dir.display());
    events(&mut r, &msgs, 100_000)
        .into_iter()
        .map(|e| match e {
            // input_filename is the path as given; strip the directory.
            Ev::Value(v, f, l, n) => Ev::Value(v, strip(f, &prefix), l, n),
            Ev::Error(m, f, l, n) => Ev::Error(m, strip(f, &prefix), l, n),
            Ev::End(f, l, n) => Ev::End(strip(f, &prefix), l, n),
            other => other,
        })
        .collect()
}

fn strip(f: crate::jq::value::Value, prefix: &str) -> crate::jq::value::Value {
    match f.as_str() {
        Some(s) => crate::jq::value::Value::from(s.strip_prefix(prefix).unwrap_or(s)),
        None => f,
    }
}

#[test]
fn compressed_inputs_read_like_their_content() {
    let dir = tempfile::tempdir().unwrap();
    let a = sample_ndjson(1, 200);
    let b = sample_ndjson(2, 150);
    let c = sample_ndjson(3, 120);
    std::fs::write(dir.path().join("plain.json"), &a).unwrap();
    std::fs::write(dir.path().join("one.json.gz"), gzip(&b)).unwrap();
    // Two gzip members, as `cat x.gz y.gz` makes: both decompress.
    let mut two = gzip(&c[..c.len() / 2]);
    two.extend(gzip(&c[c.len() / 2..]));
    std::fs::write(dir.path().join("two.json.gz"), two).unwrap();
    std::fs::write(
        dir.path().join("z.json.zst"),
        zstd::encode_all(&a[..], 3).unwrap(),
    )
    .unwrap();
    let names = ["plain.json", "one.json.gz", "two.json.gz", "z.json.zst"];
    let contents = [a.clone(), b, c, a];
    for opts in [
        ReaderOptions::default(),
        ReaderOptions {
            raw: true,
            ..Default::default()
        },
        ReaderOptions {
            slurp: true,
            ..Default::default()
        },
    ] {
        let want = from_memory(&names, &contents, opts);
        let got = from_disk(dir.path(), &names, opts);
        assert!(
            got == want,
            "{opts:?}: {} vs {} events",
            got.len(),
            want.len()
        );
    }
}

#[test]
fn stdin_redirected_from_a_file_is_mapped_from_its_offset() {
    let mut f = tempfile::tempfile().unwrap();
    f.write_all(b"1\n2\n3\n").unwrap();
    f.seek(SeekFrom::Start(2)).unwrap();
    let Opened::Whole(bytes) = open_borrowed_fd(f.as_raw_fd()).unwrap() else {
        panic!("a regular file should be mapped");
    };
    assert_eq!((*bytes).as_ref(), b"2\n3\n");
    // The offset moved to the end, as if read: `-` a second time is empty.
    let Opened::Whole(again) = open_borrowed_fd(f.as_raw_fd()).unwrap() else {
        panic!("a regular file should be mapped");
    };
    assert!((*again).as_ref().is_empty());
    // A pipe is streamed.
    let (rx, _tx) = std::io::pipe().unwrap();
    assert!(matches!(
        open_borrowed_fd(rx.as_raw_fd()).unwrap(),
        Opened::Stream { fd: Some(_), .. }
    ));
}

struct Collect(Vec<u8>, Vec<i32>);

impl RecordSink for Collect {
    fn record(&mut self, out: &[u8], _err: &[u8], status: i32) -> std::ops::ControlFlow<()> {
        self.0.extend_from_slice(out);
        self.1.push(status);
        std::ops::ControlFlow::Continue(())
    }
    fn parse_error(&mut self, e: Error) -> std::ops::ControlFlow<()> {
        self.0
            .extend_from_slice(format!("parse error: {e}\n").as_bytes());
        std::ops::ControlFlow::Break(())
    }
}

#[test]
fn engine_on_mapped_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut names = Vec::new();
    for i in 0..3 {
        let name = format!("part{i}.ndjson");
        let mut data = sample_ndjson(10 + i, 3000);
        if i == 1 {
            // Something workers hand back to the reader, then more records.
            data.extend_from_slice(b"nan\n{\"a\":\n1}\n1 2\n");
            data.extend(sample_ndjson(99, 500));
        }
        std::fs::write(dir.path().join(&name), data).unwrap();
        names.push(name);
    }
    let paths: Vec<OsString> = names
        .iter()
        .map(|n| dir.path().join(n).into_os_string())
        .collect();
    let factory = DumpFactory {
        opts: DumpOptions::compact(),
        with_position: true,
    };
    let run = |threads: usize| {
        let mut r = InputReader::new(paths.clone(), ReaderOptions::default());
        let mut sink = Collect(Vec::new(), Vec::new());
        let opts = EngineOptions {
            threads,
            min_window: 0,
            max_job_bytes: 4096,
            ..EngineOptions::default()
        };
        let stats = parallel::run(&mut r, &factory, &mut sink, &opts);
        (sink, stats)
    };
    let (want, _) = run(0);
    let (got, stats) = run(4);
    assert!(stats.worker_records > 5000, "{stats:?}");
    assert_eq!(
        String::from_utf8_lossy(&got.0),
        String::from_utf8_lossy(&want.0)
    );
    assert_eq!(got.1, want.1);
}
