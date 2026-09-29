//! Parse throughput of qj's input layer, stage by stage, next to serde_json
//! and the jq-compatible tools end to end:
//!
//! - simdjson's DOM parse through the FFI (`TapeParser`: the tape only; compare
//!   with `benches/bench_cpp`, the same parse without FFI);
//! - simdjson to jq values (`SimdParser`: the tape parse plus building values);
//! - jq's parser port (`parse_sized`), the fallback for anything simdjson rejects;
//! - for NDJSON, jq's input loop (`InputReader`) with and without the simdjson
//!   fast path.
//!
//! ```text
//! bash benches/download_data.sh --json --gharchive
//! cargo build --release          # to include qj end to end
//! cargo bench --bench parse_throughput
//! ```

use qj::io::simd::SimdParser;
use qj::io::{InputReader, Opened, Opener, ReaderOptions};
use qj::simdjson::{TapeParser, padding};
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn mb_per_sec(bytes: u64, dur: Duration) -> f64 {
    bytes as f64 / (1024.0 * 1024.0) / dur.as_secs_f64()
}

/// Runs `work` (which processes `bytes` bytes) enough times to fill about two
/// seconds, after three warmup runs that also calibrate the count.
fn bench(label: &str, bytes: usize, mut work: impl FnMut()) {
    let t = Instant::now();
    for _ in 0..3 {
        work();
    }
    let per_iter = t.elapsed().as_secs_f64() / 3.0;
    let iters = ((2.0 / per_iter.max(1e-9)) as u64).clamp(3, 1_000_000);

    let start = Instant::now();
    for _ in 0..iters {
        work();
    }
    let elapsed = start.elapsed();
    let mbs = mb_per_sec(bytes as u64 * iters, elapsed);
    println!(
        "  {label:<40} {mbs:8.1} MB/s  ({iters} iters in {:.2}s)",
        elapsed.as_secs_f64()
    );
}

/// `data` followed by simdjson's padding, so it can be parsed in place.
fn padded(data: &[u8]) -> Vec<u8> {
    let mut buf = data.to_vec();
    buf.resize(data.len() + padding(), 0);
    buf
}

/// The line ranges of an NDJSON buffer (without the newlines; blank lines
/// skipped).
fn lines(data: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for end in memchr::memchr_iter(b'\n', data).chain(std::iter::once(data.len())) {
        if end > start {
            out.push((start, end));
        }
        start = end + 1;
    }
    out
}

/// Serves one input from memory without copying it.
struct Shared(Arc<Vec<u8>>);

impl Opener for Shared {
    fn open(&mut self, _name: &OsStr) -> std::io::Result<Opened> {
        Ok(Opened::Whole(self.0.clone()))
    }
}

/// Reads every value of `data` with jq's input loop.
fn read_all(data: &Arc<Vec<u8>>, fast_path: bool) -> usize {
    let files = Shared(data.clone());
    let mut reader = InputReader::with_opener(
        vec!["input".into()],
        ReaderOptions::default(),
        Box::new(files),
    );
    reader.set_fast_path(fast_path);
    let mut n = 0;
    while let Some(value) = reader.next() {
        value.unwrap();
        n += 1;
    }
    n
}

/// Find an external tool (jq, jaq, gojq) on PATH. Returns None if not installed.
fn find_tool(name: &str) -> Option<String> {
    Command::new("which")
        .arg(name)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Benchmark a tool end to end by spawning it repeatedly:
/// `tool FILTER FILE > /dev/null`.
fn bench_external_tool(label: &str, tool: &str, filter: &str, file: &Path, file_bytes: u64) {
    let run = || {
        Command::new(tool)
            .args([filter, file.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .unwrap()
    };
    // Warmup
    for _ in 0..3 {
        let _ = run();
    }

    // Single timed run to calibrate
    let t0 = Instant::now();
    let _ = run();
    let single = t0.elapsed();

    let iters = ((2.0 / single.as_secs_f64()) as u64).max(3);

    let start = Instant::now();
    for _ in 0..iters {
        let out = run();
        assert!(out.status.success(), "{tool} failed on {}", file.display());
    }
    let elapsed = start.elapsed();
    let mbs = mb_per_sec(file_bytes * iters, elapsed);
    println!(
        "  {label:<40} {mbs:8.1} MB/s  ({iters} iters in {:.2}s)",
        elapsed.as_secs_f64()
    );
}

/// The tools to run end to end: qj (if built with `cargo build --release`),
/// then jq, jaq and gojq from PATH.
fn tools() -> Vec<(String, String)> {
    let mut tools = Vec::new();
    let qj = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/qj");
    if qj.exists() {
        tools.push(("qj".to_string(), qj.display().to_string()));
    } else {
        println!("qj end to end: SKIPPED (run cargo build --release)");
    }
    for name in ["jq", "jaq", "gojq"] {
        if let Some(path) = find_tool(name) {
            tools.push((name.to_string(), path));
        }
    }
    tools
}

fn main() {
    println!("=== qj parse throughput benchmark ===\n");

    let tools = tools();
    if !tools.is_empty() {
        let list: Vec<String> = tools.iter().map(|(n, p)| format!("{n}={p}")).collect();
        println!("Tools: {}\n", list.join(" "));
    }

    let data_dir = Path::new("benches/data");

    // --- Single-document benchmarks ---
    for fname in ["twitter.json"] {
        let path = data_dir.join(fname);
        if !path.exists() {
            println!("{fname:<40} SKIPPED (run benches/download_data.sh --json)");
            continue;
        }

        let raw = std::fs::read(&path).unwrap();
        let buf = padded(&raw);
        let len = raw.len();

        println!("{fname} ({len} bytes):");
        for (name, tool) in &tools {
            let label = format!("{name} '.' (end-to-end)");
            bench_external_tool(&label, tool, ".", &path, len as u64);
        }
        bench("serde_json DOM parse", len, || {
            let _: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        });
        let mut tape = TapeParser::new().unwrap();
        bench("simdjson DOM parse (FFI, tape only)", len, || {
            tape.parse(&buf, len).unwrap();
        });
        let mut simd = SimdParser::new();
        bench("simdjson -> jq value", len, || {
            simd.parse(&buf, 0, len).unwrap();
        });
        bench("jq parser port -> jq value", len, || {
            qj::jq::value::parse_sized(&raw).unwrap();
        });
        println!();
    }

    // --- NDJSON benchmarks ---
    for fname in ["gharchive.ndjson"] {
        let path = data_dir.join(fname);
        if !path.exists() {
            println!("{fname:<40} SKIPPED (run benches/download_data.sh --gharchive)");
            continue;
        }

        let raw = std::fs::read(&path).unwrap();
        let buf = padded(&raw);
        let len = raw.len();
        let lines = lines(&raw);
        let shared = Arc::new(raw.clone());

        println!("{fname} ({len} bytes, {} lines):", lines.len());
        for (name, tool) in &tools {
            let label = format!("{name} '.type' (end-to-end)");
            bench_external_tool(&label, tool, ".type", &path, len as u64);
        }
        bench("serde_json line by line", len, || {
            for &(s, e) in &lines {
                let _: serde_json::Value = serde_json::from_slice(&raw[s..e]).unwrap();
            }
        });
        let mut tape = TapeParser::new().unwrap();
        bench("simdjson DOM parse per line (FFI)", len, || {
            for &(s, e) in &lines {
                tape.parse(&buf[s..], e - s).unwrap();
            }
        });
        let mut simd = SimdParser::new();
        bench("simdjson -> jq value per line", len, || {
            for &(s, e) in &lines {
                simd.parse(&buf, s, e).unwrap();
            }
        });
        bench("jq input loop (simdjson fast path)", len, || {
            read_all(&shared, true);
        });
        bench("jq input loop (jq parser port)", len, || {
            read_all(&shared, false);
        });
        println!();
    }
}
