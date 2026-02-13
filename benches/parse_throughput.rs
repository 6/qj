use jx::simdjson;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn mb_per_sec(bytes: u64, dur: Duration) -> f64 {
    bytes as f64 / (1024.0 * 1024.0) / dur.as_secs_f64()
}

/// Auto-calibrate iteration count to fill ~2 seconds.
fn calibrate(bytes: usize) -> u64 {
    let iters = (2.0 * 2e9 / bytes as f64) as u64;
    iters.max(10)
}

fn bench_serde_json_parse(label: &str, data: &[u8]) {
    let iters = calibrate(data.len());

    // Warmup
    for _ in 0..3 {
        let _: serde_json::Value = serde_json::from_slice(data).unwrap();
    }

    let start = Instant::now();
    for _ in 0..iters {
        let _: serde_json::Value = serde_json::from_slice(data).unwrap();
    }
    let elapsed = start.elapsed();
    let mbs = mb_per_sec(data.len() as u64 * iters, elapsed);
    println!(
        "  {label:<35} {mbs:8.1} MB/s  ({iters} iters in {:.2}s)",
        elapsed.as_secs_f64()
    );
}

fn bench_serde_json_ndjson_parse(label: &str, data: &[u8]) {
    let iters = calibrate(data.len()).min(200);

    // Warmup
    for _ in 0..3 {
        for line in data.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let _: serde_json::Value = serde_json::from_slice(line).unwrap();
        }
    }

    let start = Instant::now();
    for _ in 0..iters {
        for line in data.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let _: serde_json::Value = serde_json::from_slice(line).unwrap();
        }
    }
    let elapsed = start.elapsed();
    let mbs = mb_per_sec(data.len() as u64 * iters, elapsed);
    println!(
        "  {label:<35} {mbs:8.1} MB/s  ({iters} iters in {:.2}s)",
        elapsed.as_secs_f64()
    );
}

fn bench_simdjson_ondemand_parse(label: &str, padded: &[u8], json_len: usize) {
    let iters = calibrate(json_len);
    let mut parser = simdjson::Parser::new().unwrap();

    // Warmup
    for _ in 0..3 {
        let _doc = parser.parse(padded, json_len).unwrap();
    }

    let start = Instant::now();
    for _ in 0..iters {
        let _doc = parser.parse(padded, json_len).unwrap();
    }
    let elapsed = start.elapsed();
    let mbs = mb_per_sec(json_len as u64 * iters, elapsed);
    println!(
        "  {label:<35} {mbs:8.1} MB/s  ({iters} iters in {:.2}s)",
        elapsed.as_secs_f64()
    );
}

fn bench_simdjson_ondemand_field(label: &str, padded: &[u8], json_len: usize, field: &str) {
    let iters = calibrate(json_len);
    let mut parser = simdjson::Parser::new().unwrap();

    // Verify field exists
    {
        let mut doc = parser.parse(padded, json_len).unwrap();
        if doc.find_field_str(field).is_err() {
            println!("  {label:<35} SKIPPED (field '{field}' not found)");
            return;
        }
    }

    let start = Instant::now();
    for _ in 0..iters {
        let mut doc = parser.parse(padded, json_len).unwrap();
        let _ = doc.find_field_str(field).unwrap();
    }
    let elapsed = start.elapsed();
    let mbs = mb_per_sec(json_len as u64 * iters, elapsed);
    println!(
        "  {label:<35} {mbs:8.1} MB/s  ({iters} iters in {:.2}s)",
        elapsed.as_secs_f64()
    );
}

fn bench_iterate_many_count(label: &str, padded: &[u8], json_len: usize) {
    let iters = calibrate(json_len).min(200);

    // Warmup
    for _ in 0..3 {
        let _ = simdjson::iterate_many_count(padded, json_len, 1_000_000).unwrap();
    }

    let start = Instant::now();
    let mut total_docs: u64 = 0;
    for _ in 0..iters {
        total_docs += simdjson::iterate_many_count(padded, json_len, 1_000_000).unwrap();
    }
    let elapsed = start.elapsed();
    let mbs = mb_per_sec(json_len as u64 * iters, elapsed);
    println!(
        "  {label:<35} {mbs:8.1} MB/s  ({iters} iters, {total_docs} docs total, {:.2}s)",
        elapsed.as_secs_f64()
    );
}

fn bench_iterate_many_extract(label: &str, padded: &[u8], json_len: usize, field: &str) {
    let iters = calibrate(json_len).min(200);

    // Warmup
    for _ in 0..3 {
        let _ = simdjson::iterate_many_extract_field(padded, json_len, 1_000_000, field).unwrap();
    }

    let start = Instant::now();
    let mut total_bytes: u64 = 0;
    for _ in 0..iters {
        total_bytes +=
            simdjson::iterate_many_extract_field(padded, json_len, 1_000_000, field).unwrap();
    }
    let elapsed = start.elapsed();
    let mbs = mb_per_sec(json_len as u64 * iters, elapsed);
    println!(
        "  {label:<35} {mbs:8.1} MB/s  ({iters} iters, {total_bytes} bytes extracted, {:.2}s)",
        elapsed.as_secs_f64()
    );
}

/// Find an external tool (jq, jaq) on PATH. Returns None if not installed.
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

/// Benchmark an external JSON tool (jq/jaq) by spawning it repeatedly.
/// Uses `tool '.' file > /dev/null` to measure parse + identity filter throughput.
fn bench_external_tool(label: &str, tool: &str, filter: &str, file: &Path, file_bytes: u64) {
    // Calibrate: run once to estimate per-invocation time, then pick iters for ~2s total.
    // Warmup
    for _ in 0..3 {
        let _ = Command::new(tool)
            .args([filter, file.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
    }

    // Single timed run to calibrate
    let t0 = Instant::now();
    let _ = Command::new(tool)
        .args([filter, file.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let single = t0.elapsed();

    let iters = ((2.0 / single.as_secs_f64()) as u64).max(3);

    let start = Instant::now();
    for _ in 0..iters {
        let out = Command::new(tool)
            .args([filter, file.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{tool} failed on {}", file.display());
    }
    let elapsed = start.elapsed();
    let mbs = mb_per_sec(file_bytes * iters, elapsed);
    println!(
        "  {label:<35} {mbs:8.1} MB/s  ({iters} iters in {:.2}s)",
        elapsed.as_secs_f64()
    );
}

fn main() {
    println!("=== jx parse throughput benchmark ===\n");

    let jq_path = find_tool("jq");
    let jaq_path = find_tool("jaq");
    let gojq_path = find_tool("gojq");
    let has_external = jq_path.is_some() || jaq_path.is_some() || gojq_path.is_some();
    if has_external {
        print!("External tools:");
        if let Some(p) = &jq_path {
            print!(" jq={p}");
        }
        if let Some(p) = &jaq_path {
            print!(" jaq={p}");
        }
        if let Some(p) = &gojq_path {
            print!(" gojq={p}");
        }
        println!("\n");
    }

    let data_dir = Path::new("bench/data");

    // --- Single-file benchmarks ---
    // (field, None) = no top-level string field to test find_field_str against.
    let files: &[(&str, Option<&str>)] = &[
        ("twitter.json", None),        // top-level fields are objects
        ("citm_catalog.json", None),   // top-level fields are objects
        ("canada.json", Some("type")), // "type": "FeatureCollection"
    ];

    for &(fname, field) in files {
        let path = data_dir.join(fname);
        if !path.exists() {
            println!("{fname:<40} SKIPPED (run bench/download_testdata.sh)");
            continue;
        }

        let raw = std::fs::read(&path).unwrap();
        let padded = simdjson::read_padded(&path).unwrap();
        let json_len = raw.len();

        println!("{fname} ({json_len} bytes):");
        if let Some(ref jq) = jq_path {
            bench_external_tool("jq '.' (end-to-end)", jq, ".", &path, json_len as u64);
        }
        if let Some(ref jaq) = jaq_path {
            bench_external_tool("jaq '.' (end-to-end)", jaq, ".", &path, json_len as u64);
        }
        if let Some(ref gojq) = gojq_path {
            bench_external_tool("gojq '.' (end-to-end)", gojq, ".", &path, json_len as u64);
        }
        bench_serde_json_parse("serde_json DOM parse", &raw);
        bench_simdjson_ondemand_parse("simdjson On-Demand parse (FFI)", &padded, json_len);
        if let Some(f) = field {
            bench_simdjson_ondemand_field(
                &format!("simdjson find_field(\"{f}\") (FFI)"),
                &padded,
                json_len,
                f,
            );
        }
        println!();
    }

    // --- NDJSON benchmarks ---
    let ndjson_files = &["100k.ndjson", "1m.ndjson"];

    for &fname in ndjson_files {
        let path = data_dir.join(fname);
        if !path.exists() {
            println!("{fname:<40} SKIPPED (run bench/generate_ndjson.sh)");
            continue;
        }

        let raw = std::fs::read(&path).unwrap();
        let padded = simdjson::read_padded(&path).unwrap();
        let json_len = raw.len();

        println!("{fname} ({json_len} bytes):");
        if let Some(ref jq) = jq_path {
            bench_external_tool(
                "jq '.name' (end-to-end)",
                jq,
                ".name",
                &path,
                json_len as u64,
            );
        }
        if let Some(ref jaq) = jaq_path {
            bench_external_tool(
                "jaq '.name' (end-to-end)",
                jaq,
                ".name",
                &path,
                json_len as u64,
            );
        }
        if let Some(ref gojq) = gojq_path {
            bench_external_tool(
                "gojq '.name' (end-to-end)",
                gojq,
                ".name",
                &path,
                json_len as u64,
            );
        }
        bench_serde_json_ndjson_parse("serde_json NDJSON line-by-line", &raw);
        bench_iterate_many_count("iterate_many count (FFI)", &padded, json_len);
        bench_iterate_many_extract(
            "iterate_many extract(\"name\") (FFI)",
            &padded,
            json_len,
            "name",
        );
        println!();
    }
}
