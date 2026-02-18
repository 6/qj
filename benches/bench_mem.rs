use clap::Parser;
use std::collections::HashMap;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

// --- CLI ---

#[derive(Parser)]
#[command(about = "Measure peak memory (RSS) of qj vs jq vs jaq vs gojq.")]
struct Args {
    /// Benchmark type: "json" or "ndjson"
    #[arg(long = "type")]
    benchmark_type: String,

    /// Number of runs per tool/filter combination (reports median)
    #[arg(long, default_value_t = 3)]
    runs: u32,

    /// Output markdown file path (default: derived from --type)
    #[arg(long)]
    output: Option<PathBuf>,

    /// Override the input file path
    #[arg(long)]
    file: Option<PathBuf>,

    /// Skip the single-thread (1T) qj variant (useful on single-core CI runners)
    #[arg(long)]
    no_1t: bool,
}

impl Args {
    fn output_path(&self) -> PathBuf {
        if let Some(ref p) = self.output {
            p.clone()
        } else {
            match self.benchmark_type.as_str() {
                "ndjson" => PathBuf::from("benches/results_mem_ndjson.md"),
                _ => PathBuf::from("benches/results_mem_json.md"),
            }
        }
    }
}

// --- Filter definitions ---

struct BenchFilter {
    name: &'static str,
    flags: &'static [&'static str],
    expr: &'static str,
}

fn filter_display(f: &BenchFilter) -> String {
    let mut s = String::new();
    for flag in f.flags {
        write!(s, "{flag} ").unwrap();
    }
    write!(s, "'{}'", f.expr).unwrap();
    s
}

static JSON_FILTERS: &[BenchFilter] = &[
    BenchFilter {
        name: "identity compact",
        flags: &["-c"],
        expr: ".",
    },
    BenchFilter {
        name: "field access",
        flags: &["-c"],
        expr: ".statuses[0].user.screen_name",
    },
    BenchFilter {
        name: "aggregation",
        flags: &[],
        expr: "[.statuses[] | .user.screen_name] | unique | length",
    },
    BenchFilter {
        name: "map + reshape",
        flags: &["-c"],
        expr: ".statuses | map({user: .user.screen_name, rt: .retweet_count})",
    },
];

// gharchive.ndjson fields
static NDJSON_GHARCHIVE_FILTERS: &[BenchFilter] = &[
    BenchFilter {
        name: "field",
        flags: &["-c"],
        expr: ".type",
    },
    BenchFilter {
        name: "select + field",
        flags: &["-c"],
        expr: r#"select(.type == "PushEvent") | .actor.login"#,
    },
    BenchFilter {
        name: "reshape",
        flags: &["-c"],
        expr: "{type, repo: .repo.name, actor: .actor.login}",
    },
];

// --- Tool discovery ---

struct Tool {
    name: String,
    path: String,
    version: String,
    extra_args: Vec<String>,
}

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

fn tool_version(path: &str) -> String {
    Command::new(path)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout);
            let err = String::from_utf8_lossy(&o.stderr);
            let s = if out.trim().is_empty() {
                err.trim().to_string()
            } else {
                out.trim().to_string()
            };
            s.lines().next().unwrap_or("").to_string()
        })
        .unwrap_or_default()
}

fn format_tool_versions(tools: &[Tool]) -> String {
    let versions: Vec<String> = tools
        .iter()
        .filter(|t| !t.name.contains("(1T)"))
        .map(|t| {
            if t.version.is_empty() {
                t.name.clone()
            } else {
                t.version.clone()
            }
        })
        .collect();
    format!("Tools: {}", versions.join(", "))
}

fn discover_tools(qj_path: &str, include_1t: bool) -> Vec<Tool> {
    let qj_version = tool_version(qj_path);
    let mut tools = vec![Tool {
        name: "qj".into(),
        path: qj_path.into(),
        version: qj_version.clone(),
        extra_args: vec![],
    }];
    if include_1t {
        tools.push(Tool {
            name: "qj (1T)".into(),
            path: qj_path.into(),
            version: qj_version,
            extra_args: vec!["--threads".into(), "1".into()],
        });
    }
    match find_tool("jq") {
        Some(path) => {
            let version = tool_version(&path);
            tools.push(Tool {
                name: "jq".into(),
                path,
                version,
                extra_args: vec![],
            });
        }
        None => {
            eprintln!("Error: jq not found.");
            std::process::exit(1);
        }
    }
    for name in ["jaq", "gojq"] {
        if let Some(path) = find_tool(name) {
            let version = tool_version(&path);
            tools.push(Tool {
                name: name.into(),
                path,
                version,
                extra_args: vec![],
            });
        } else {
            eprintln!("Note: {name} not found, skipping");
        }
    }
    tools
}

// --- Helpers ---

fn shell_output(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn detect_platform() -> String {
    let os = shell_output("uname", &["-s"]);
    if os.trim().eq_ignore_ascii_case("darwin") {
        let chip = Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "Apple Silicon".to_string());
        let ram = Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|bytes| format!("{} GB", bytes / (1024 * 1024 * 1024)))
            .unwrap_or_default();
        if ram.is_empty() {
            chip
        } else {
            format!("{chip} ({ram})")
        }
    } else {
        let arch = shell_output("uname", &["-m"]);
        format!("{}-{}", os.to_lowercase(), arch)
    }
}

// --- Memory measurement ---

/// Normalize `ru_maxrss` to bytes. macOS reports bytes; Linux reports kilobytes.
fn maxrss_to_bytes(raw: i64) -> u64 {
    if cfg!(target_os = "linux") {
        (raw * 1024) as u64
    } else {
        // macOS, FreeBSD: already in bytes
        raw as u64
    }
}

/// Spawn a tool, wait for it via `wait4()`, return peak RSS in bytes.
fn spawn_and_get_maxrss(tool: &Tool, filter: &BenchFilter, file: &Path) -> Option<u64> {
    let mut cmd = Command::new(&tool.path);
    for arg in &tool.extra_args {
        cmd.arg(arg);
    }
    for flag in filter.flags {
        cmd.arg(flag);
    }
    cmd.arg(filter.expr).arg(file);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());

    let child = cmd.spawn().ok()?;
    let pid = child.id() as libc::pid_t;

    let mut status: libc::c_int = 0;
    let mut rusage: libc::rusage = unsafe { std::mem::zeroed() };

    let ret = unsafe { libc::wait4(pid, &mut status, 0, &mut rusage) };

    if ret < 0 {
        // wait4 failed; drop child to reap via normal waitpid
        drop(child);
        return None;
    }

    // Child is already reaped by wait4. We must prevent Child::drop from calling
    // waitpid again. Since stdout/stderr are Stdio::null() (no pipe fds to leak),
    // forget is safe here.
    std::mem::forget(child);

    Some(maxrss_to_bytes(rusage.ru_maxrss))
}

fn format_mb(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.2} GB", mb / 1024.0)
    } else {
        format!("{:.1} MB", mb)
    }
}

// --- Result storage ---

type ResultKey = (String, String); // (filter_name, tool_name)
type Results = HashMap<ResultKey, u64>; // peak RSS in bytes

// --- Benchmark runner ---

fn run_benchmarks(tools: &[Tool], filters: &[BenchFilter], file: &Path, runs: u32) -> Results {
    let mut results = Results::new();

    for filter in filters {
        eprintln!("--- {} ---", filter.name);
        for tool in tools {
            let mut measurements = Vec::with_capacity(runs as usize);
            for run_idx in 0..runs {
                eprint!("  {} run {}/{}...", tool.name, run_idx + 1, runs);
                match spawn_and_get_maxrss(tool, filter, file) {
                    Some(rss) => {
                        eprintln!(" {}", format_mb(rss));
                        measurements.push(rss);
                    }
                    None => {
                        eprintln!(" FAILED");
                    }
                }
            }
            if !measurements.is_empty() {
                measurements.sort();
                let median = measurements[measurements.len() / 2];
                results.insert((filter.name.to_string(), tool.name.clone()), median);
            }
        }
        eprintln!();
    }

    results
}

// --- Markdown generation ---

#[allow(clippy::too_many_arguments)]
fn generate_markdown(
    tools: &[Tool],
    filters: &[BenchFilter],
    results: &Results,
    file_name: &str,
    file_bytes: u64,
    runs: u32,
    platform: &str,
    date: &str,
    elapsed: std::time::Duration,
    benchmark_type: &str,
) -> String {
    let mut md = String::new();
    let type_label = match benchmark_type {
        "ndjson" => "NDJSON",
        _ => "JSON",
    };

    writeln!(md, "# Memory Usage \u{2014} {type_label}").unwrap();
    writeln!(md).unwrap();
    writeln!(
        md,
        "> Auto-generated by `cargo run --release --bin bench_mem -- --type {benchmark_type}`. Do not edit manually."
    )
    .unwrap();
    writeln!(
        md,
        "> Last updated: {date} on `{platform}` (total time: {elapsed:.0?})"
    )
    .unwrap();
    writeln!(md, "> {}", format_tool_versions(tools)).unwrap();
    writeln!(md).unwrap();

    let file_size_mb = file_bytes as f64 / (1024.0 * 1024.0);
    writeln!(
        md,
        "Input: `{file_name}` ({file_size_mb:.0} MB). Median peak RSS of {runs} runs via `wait4()` rusage."
    )
    .unwrap();
    writeln!(md).unwrap();

    // Table header
    let mut header = String::from("| Filter |");
    let mut separator = String::from("|--------|");
    for tool in tools {
        if tool.name == "qj" {
            write!(header, " **{}** |", tool.name).unwrap();
        } else {
            write!(header, " {} |", tool.name).unwrap();
        }
        write!(separator, "------:|").unwrap();
        if tool.name.starts_with("qj") {
            header.push_str(" vs jq |");
            write!(separator, "------:|").unwrap();
        }
    }
    writeln!(md, "{header}").unwrap();
    writeln!(md, "{separator}").unwrap();

    // Data rows
    for filter in filters {
        let display = filter_display(filter);
        let mut row = format!("| `{display}` |");
        let jq_rss = results.get(&(filter.name.to_string(), "jq".to_string()));
        for tool in tools {
            let rss = results.get(&(filter.name.to_string(), tool.name.clone()));
            let formatted = match rss {
                Some(&bytes) => format_mb(bytes),
                None => "-".to_string(),
            };
            if tool.name == "qj" {
                write!(row, " **{formatted}** |").unwrap();
            } else {
                write!(row, " {formatted} |").unwrap();
            }
            // vs jq column for qj variants
            if tool.name.starts_with("qj") {
                let ratio = match (jq_rss, rss) {
                    (Some(&jq_bytes), Some(&tool_bytes)) if jq_bytes > 0 && tool_bytes > 0 => {
                        format!("{:.2}x", tool_bytes as f64 / jq_bytes as f64)
                    }
                    _ => "-".to_string(),
                };
                if tool.name == "qj" {
                    write!(row, " **{ratio}** |").unwrap();
                } else {
                    write!(row, " {ratio} |").unwrap();
                }
            }
        }
        writeln!(md, "{row}").unwrap();
    }

    // Summary: median RSS across all filters per tool
    writeln!(md).unwrap();
    writeln!(md, "### Summary").unwrap();
    writeln!(md).unwrap();
    writeln!(md, "| Tool | Median peak RSS | vs jq |").unwrap();
    writeln!(md, "|------|----------------:|------:|").unwrap();

    let jq_median = {
        let mut vals: Vec<u64> = filters
            .iter()
            .filter_map(|f| {
                results
                    .get(&(f.name.to_string(), "jq".to_string()))
                    .copied()
            })
            .collect();
        vals.sort();
        if vals.is_empty() {
            0
        } else {
            vals[vals.len() / 2]
        }
    };

    for tool in tools {
        let mut vals: Vec<u64> = filters
            .iter()
            .filter_map(|f| {
                results
                    .get(&(f.name.to_string(), tool.name.clone()))
                    .copied()
            })
            .collect();
        vals.sort();
        if vals.is_empty() {
            continue;
        }
        let median = vals[vals.len() / 2];
        let formatted = format_mb(median);
        let ratio = if jq_median > 0 {
            format!("{:.2}x", median as f64 / jq_median as f64)
        } else {
            "-".to_string()
        };
        if tool.name == "qj" {
            writeln!(md, "| **{}** | **{formatted}** | **{ratio}** |", tool.name).unwrap();
        } else {
            writeln!(md, "| {} | {formatted} | {ratio} |", tool.name).unwrap();
        }
    }

    writeln!(md).unwrap();
    writeln!(md, "Lower is better. \"vs jq\" = tool\\_RSS / jq\\_RSS.").unwrap();

    md
}

// --- NDJSON file resolution ---

fn resolve_ndjson_file(data_dir: &Path) -> (PathBuf, &'static [BenchFilter]) {
    let gharchive = data_dir.join("gharchive.ndjson");
    if gharchive.exists() {
        return (gharchive, NDJSON_GHARCHIVE_FILTERS);
    }
    eprintln!(
        "Error: gharchive.ndjson not found in {}/.",
        data_dir.display()
    );
    eprintln!("Run: bash benches/download_data.sh --gharchive");
    std::process::exit(1);
}

// --- main ---

fn main() {
    let args = Args::parse();
    let data_dir = PathBuf::from("benches/data");

    // Preflight: check qj binary
    let qj_path = "target/release/qj";
    if !Path::new(qj_path).exists() {
        eprintln!("Error: {qj_path} not found. Run: cargo build --release");
        std::process::exit(1);
    }

    let tools = discover_tools(qj_path, !args.no_1t);
    eprintln!(
        "Tools: {}",
        tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let platform = detect_platform();
    let date = shell_output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]);
    eprintln!("Platform: {platform}");
    eprintln!("Date: {date}");
    eprintln!();

    let bench_start = Instant::now();

    let (file, filters, benchmark_type) = match args.benchmark_type.as_str() {
        "json" => {
            let file = if let Some(ref f) = args.file {
                f.clone()
            } else {
                let f = data_dir.join("large_twitter.json");
                if !f.exists() {
                    eprintln!("Error: {f:?} not found.");
                    eprintln!(
                        "Run: bash benches/download_data.sh --json && bash benches/generate_data.sh --json"
                    );
                    std::process::exit(1);
                }
                f
            };
            (file, JSON_FILTERS, "json")
        }
        "ndjson" => {
            let (file, ndjson_filters) = if let Some(ref f) = args.file {
                (f.clone(), NDJSON_GHARCHIVE_FILTERS)
            } else {
                resolve_ndjson_file(&data_dir)
            };
            (file, ndjson_filters, "ndjson")
        }
        other => {
            eprintln!("Error: unknown benchmark type '{other}'. Use 'json' or 'ndjson'.");
            std::process::exit(1);
        }
    };

    let file_bytes = fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    let file_name = file.file_name().unwrap_or_default().to_string_lossy();
    eprintln!(
        "Input: {} ({:.0} MB)",
        file_name,
        file_bytes as f64 / (1024.0 * 1024.0)
    );
    eprintln!("Runs: {}", args.runs);
    eprintln!();

    let results = run_benchmarks(&tools, filters, &file, args.runs);
    let elapsed = bench_start.elapsed();

    let md = generate_markdown(
        &tools,
        filters,
        &results,
        &file_name,
        file_bytes,
        args.runs,
        &platform,
        &date,
        elapsed,
        benchmark_type,
    );

    let output_path = args.output_path();
    fs::write(&output_path, &md).unwrap_or_else(|e| {
        eprintln!("Error writing {}: {e}", output_path.display());
        std::process::exit(1);
    });
    eprintln!("Results written to {}", output_path.display());

    // Print summary to stderr
    eprintln!();
    eprintln!("=== Summary ===");
    for tool in &tools {
        let mut vals: Vec<u64> = filters
            .iter()
            .filter_map(|f| {
                results
                    .get(&(f.name.to_string(), tool.name.clone()))
                    .copied()
            })
            .collect();
        vals.sort();
        if !vals.is_empty() {
            let median = vals[vals.len() / 2];
            eprintln!("  {:<10} {}", tool.name, format_mb(median));
        }
    }
    eprintln!("Total time: {elapsed:.0?}");
}
