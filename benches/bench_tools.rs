use clap::Parser;
use std::collections::HashMap;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

// --- CLI ---

#[derive(Parser)]
#[command(about = "Benchmark qj vs jq vs jaq vs gojq. Writes results to markdown.")]
struct Args {
    /// Benchmark type: "json" (single-file) or "ndjson" (GH Archive parallel)
    #[arg(long = "type")]
    benchmark_type: String,

    /// Seconds to sleep between benchmark groups (mitigates thermal throttling)
    #[arg(long, default_value_t = 5)]
    cooldown: u64,

    /// Number of hyperfine runs per benchmark
    #[arg(long, default_value_t = 3)]
    runs: u32,

    /// Output markdown file path (default: derived from --type)
    #[arg(long)]
    output: Option<PathBuf>,

    /// Run only these benchmark groups.
    /// Can be repeated. Omit to run all.
    #[arg(long)]
    only: Vec<String>,

    /// Skip the correctness check phase
    #[arg(long)]
    skip_correctness: bool,

    /// NDJSON dataset size: "small" (1.1GB), "medium" (3.4GB), "large" (6.2GB)
    #[arg(long, default_value = "medium")]
    size: String,
}

impl Args {
    fn should_run(&self, group: &str) -> bool {
        self.only.is_empty() || self.only.iter().any(|g| g == group)
    }

    fn output_path(&self) -> PathBuf {
        if let Some(ref p) = self.output {
            p.clone()
        } else {
            match self.benchmark_type.as_str() {
                "ndjson" => PathBuf::from(format!("benches/results_ndjson_{}.md", self.size)),
                _ => PathBuf::from("benches/results_json.md"),
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

static JSON_FILTERS: &[BenchFilter] = &[
    BenchFilter {
        name: "identity compact",
        flags: &["-c"],
        expr: ".",
    },
    BenchFilter {
        name: "field extraction",
        flags: &["-c"],
        expr: ".statuses",
    },
    BenchFilter {
        name: "pipe + length",
        flags: &[],
        expr: ".statuses|length",
    },
    BenchFilter {
        name: "keys",
        flags: &["-c"],
        expr: "keys",
    },
    BenchFilter {
        name: "iterate + field",
        flags: &[],
        expr: ".statuses[]|.user.name",
    },
    BenchFilter {
        name: "iterate + field (compact)",
        flags: &["-c"],
        expr: ".statuses[]|.user.name",
    },
    BenchFilter {
        name: "map + field (compact)",
        flags: &["-c"],
        expr: ".statuses|map(.user)",
    },
    BenchFilter {
        name: "map + fields obj (compact)",
        flags: &["-c"],
        expr: ".statuses|map({user, text})",
    },
    BenchFilter {
        name: "map + type (compact)",
        flags: &["-c"],
        expr: ".statuses|map(type)",
    },
    BenchFilter {
        name: "map + length (compact)",
        flags: &["-c"],
        expr: ".statuses|map(length)",
    },
    BenchFilter {
        name: "select + construct",
        flags: &[],
        expr: ".statuses[]|select(.retweet_count>0)|{user:.user.screen_name,n:.retweet_count}",
    },
    BenchFilter {
        name: "math (floor)",
        flags: &[],
        expr: "[.statuses[]|.retweet_count|floor]",
    },
    BenchFilter {
        name: "string ops (split+join)",
        flags: &[],
        expr: r#"[.statuses[]|.user.screen_name|split("_")|join("-")]"#,
    },
    BenchFilter {
        name: "unique + sort",
        flags: &[],
        expr: "[.statuses[]|.user.screen_name]|unique|length",
    },
    BenchFilter {
        name: "paths(scalars)",
        flags: &[],
        expr: "[paths(scalars)]|length",
    },
    BenchFilter {
        name: "map_values + tojson",
        flags: &[],
        expr: ".statuses[0]|map_values(tojson)",
    },
    // Phase 2 language features
    BenchFilter {
        name: "reduce (sum)",
        flags: &[],
        expr: "reduce .statuses[] as $s (0; . + $s.retweet_count)",
    },
    BenchFilter {
        name: "variable binding",
        flags: &[],
        expr: ".statuses[] | . as $s | {name: $s.user.screen_name, rts: $s.retweet_count}",
    },
    BenchFilter {
        name: "slicing",
        flags: &[],
        expr: "[.statuses[].user.screen_name][:5]",
    },
    BenchFilter {
        name: "try (error suppression)",
        flags: &[],
        expr: "[.statuses[] | try (1 / .retweet_count)]",
    },
    BenchFilter {
        name: "elif",
        flags: &[],
        expr: r#"[.statuses[] | if .retweet_count > 10 then "viral" elif .retweet_count > 0 then "shared" else "original" end]"#,
    },
    BenchFilter {
        name: "walk",
        flags: &["-c"],
        expr: r#"walk(if type == "boolean" then not else . end)"#,
    },
    // Assignment operators
    BenchFilter {
        name: "update assign (|=)",
        flags: &["-c"],
        expr: ".statuses[0].retweet_count |= . + 1",
    },
    BenchFilter {
        name: "arithmetic assign (+=)",
        flags: &["-c"],
        expr: ".statuses[] |= (.retweet_count += 1)",
    },
    BenchFilter {
        name: "regex (gsub)",
        flags: &["-c"],
        expr: r#"[.statuses[]|.user.screen_name|gsub("_"; "-")]"#,
    },
    BenchFilter {
        name: "string interpolation",
        flags: &["-c"],
        expr: r#"[.statuses[]|"@\(.user.screen_name): \(.text[0:30])"]"#,
    },
    BenchFilter {
        name: "format (@base64)",
        flags: &["-c"],
        expr: r#"[.statuses[]|.user.screen_name|@base64]"#,
    },
    BenchFilter {
        name: "group_by",
        flags: &[],
        expr: ".statuses | group_by(.user.screen_name) | length",
    },
    BenchFilter {
        name: "sort_by",
        flags: &["-c"],
        expr: ".statuses | sort_by(.retweet_count) | .[-1].user.screen_name",
    },
    // User-defined functions
    BenchFilter {
        name: "def (user func)",
        flags: &["-c"],
        expr: r#"def hi(rt): if rt > 10 then "viral" elif rt > 0 then "shared" else "none" end; [.statuses[] | hi(.retweet_count)]"#,
    },
];

static NDJSON_FILTERS: &[BenchFilter] = &[
    BenchFilter {
        name: "field",
        flags: &[],
        expr: ".actor.login",
    },
    BenchFilter {
        name: "length",
        flags: &["-c"],
        expr: "length",
    },
    BenchFilter {
        name: "keys",
        flags: &["-c"],
        expr: "keys",
    },
    BenchFilter {
        name: "type",
        flags: &["-c"],
        expr: "type",
    },
    BenchFilter {
        name: "has",
        flags: &["-c"],
        expr: r#"has("actor")"#,
    },
    BenchFilter {
        name: "select",
        flags: &["-c"],
        expr: r#"select(.type == "PushEvent")"#,
    },
    BenchFilter {
        name: "select+field",
        flags: &["-c"],
        expr: r#"select(.type == "PushEvent") | .payload.size"#,
    },
    BenchFilter {
        name: "reshape",
        flags: &["-c"],
        expr: "{type, repo: .repo.name, actor: .actor.login}",
    },
    BenchFilter {
        name: "evaluator",
        flags: &["-c"],
        expr: "{type, commits: [.payload.commits[]?.message]}",
    },
    BenchFilter {
        name: "evaluator (complex)",
        flags: &["-c"],
        expr: "{type, commits: (.payload.commits // [] | length)}",
    },
];

/// Subset of NDJSON filters for medium/large datasets (faster to run).
static NDJSON_FILTERS_SHORT: &[BenchFilter] = &[
    BenchFilter {
        name: "field",
        flags: &[],
        expr: ".actor.login",
    },
    BenchFilter {
        name: "select",
        flags: &["-c"],
        expr: r#"select(.type == "PushEvent")"#,
    },
    BenchFilter {
        name: "reshape",
        flags: &["-c"],
        expr: "{type, repo: .repo.name, actor: .actor.login}",
    },
    BenchFilter {
        name: "evaluator",
        flags: &["-c"],
        expr: "{type, commits: [.payload.commits[]?.message]}",
    },
    BenchFilter {
        name: "evaluator (complex)",
        flags: &["-c"],
        expr: "{type, commits: (.payload.commits // [] | length)}",
    },
];

// --- Tool discovery ---

struct Tool {
    name: String,
    path: String,
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

fn discover_tools(qj_path: &str) -> Vec<Tool> {
    let mut tools = vec![
        Tool {
            name: "qj".into(),
            path: qj_path.into(),
            extra_args: vec![],
        },
        Tool {
            name: "qj (1T)".into(),
            path: qj_path.into(),
            extra_args: vec!["--threads".into(), "1".into()],
        },
    ];
    match find_tool("jq") {
        Some(path) => tools.push(Tool {
            name: "jq".into(),
            path,
            extra_args: vec![],
        }),
        None => {
            eprintln!("Error: jq not found.");
            std::process::exit(1);
        }
    }
    for name in ["jaq", "gojq"] {
        if let Some(path) = find_tool(name) {
            tools.push(Tool {
                name: name.into(),
                path,
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

/// Build a shell command string for hyperfine (run via default shell).
fn build_cmd(tool: &Tool, flags: &[&str], expr: &str, file: &str) -> String {
    let mut cmd = tool.path.to_string();
    for arg in &tool.extra_args {
        write!(cmd, " {arg}").unwrap();
    }
    for flag in flags {
        write!(cmd, " {flag}").unwrap();
    }
    write!(cmd, " '{expr}' '{file}'").unwrap();
    cmd
}

/// Check if a tool can run a filter on a file without error.
fn tool_supports_filter(tool: &Tool, filter: &BenchFilter, file: &Path) -> bool {
    let mut cmd = Command::new(&tool.path);
    for arg in &tool.extra_args {
        cmd.arg(arg);
    }
    for flag in filter.flags {
        cmd.arg(flag);
    }
    cmd.arg(filter.expr)
        .arg(file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run a tool and capture stdout+stderr combined (for correctness comparison on small files).
fn run_tool_output(tool: &Tool, filter: &BenchFilter, file: &Path) -> String {
    let mut cmd = Command::new(&tool.path);
    for arg in &tool.extra_args {
        cmd.arg(arg);
    }
    for flag in filter.flags {
        cmd.arg(flag);
    }
    cmd.arg(filter.expr).arg(file);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    match cmd.output() {
        Ok(output) => {
            let mut out = String::from_utf8_lossy(&output.stdout).into_owned();
            out.push_str(&String::from_utf8_lossy(&output.stderr));
            out
        }
        Err(e) => format!("ERROR: {e}"),
    }
}

fn format_time(seconds: f64) -> String {
    let ms = seconds * 1000.0;
    if ms >= 1000.0 {
        format!("{seconds:.2}s")
    } else if ms < 0.1 {
        "<0.1ms".to_string()
    } else {
        format!("{ms:.1}ms")
    }
}

fn format_throughput(bytes: u64, seconds: f64) -> String {
    let mbps = bytes as f64 / seconds / (1024.0 * 1024.0);
    if mbps >= 1024.0 {
        format!("{:.1} GB/s", mbps / 1024.0)
    } else {
        format!("{mbps:.0} MB/s")
    }
}

/// Format a result value as time string, appending `*` if any runs had non-zero exit codes.
fn format_result(val: Option<&ResultValue>) -> String {
    match val {
        Some(&(seconds, failed)) => {
            let t = format_time(seconds);
            if failed { format!("{t}*") } else { t }
        }
        None => "-".to_string(),
    }
}

/// Format a throughput value, appending `*` if any runs had non-zero exit codes.
fn format_throughput_result(val: Option<&ResultValue>, bytes: u64) -> String {
    match val {
        Some(&(seconds, failed)) => {
            let t = format_throughput(bytes, seconds);
            if failed { format!("{t}*") } else { t }
        }
        None => "-".to_string(),
    }
}

/// Geometric mean of jq_time/tool_time ratios.
fn geomean_ratio(pairs: &[(f64, f64)]) -> Option<f64> {
    let valid: Vec<_> = pairs.iter().filter(|(a, b)| *a > 0.0 && *b > 0.0).collect();
    if valid.is_empty() {
        return None;
    }
    let sum_log: f64 = valid
        .iter()
        .map(|(jq_time, tool_time)| (jq_time / tool_time).ln())
        .sum();
    Some((sum_log / valid.len() as f64).exp())
}

fn filter_display(filter: &BenchFilter) -> String {
    if filter.flags.is_empty() {
        format!("'{}'", filter.expr)
    } else {
        format!("{} '{}'", filter.flags.join(" "), filter.expr)
    }
}

fn is_macos() -> bool {
    std::env::consts::OS == "macos"
}

// --- Result storage ---

/// (median_seconds, had_nonzero_exit_code)
type ResultValue = (f64, bool);
type ResultKey = (String, String, String); // (filter_key, file, tool)
type Results = HashMap<ResultKey, ResultValue>;

fn result_key(filter_key: &str, file: &str, tool: &str) -> ResultKey {
    (filter_key.into(), file.into(), tool.into())
}

// --- Benchmark runner ---

#[allow(clippy::too_many_arguments)]
fn run_benchmarks(
    tools: &[Tool],
    filters: &[BenchFilter],
    key_prefix: &str,
    files: &[&str],
    data_dir: &Path,
    results_dir: &Path,
    args: &Args,
    results: &mut Results,
    use_caffeinate: bool,
    ignore_failure: bool,
) {
    for file in files {
        let file_path = data_dir.join(file);
        let file_bytes = fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
        eprintln!("=== {file} ({file_bytes} bytes) ===");
        eprintln!();

        for (i, filter) in filters.iter().enumerate() {
            let filter_key = format!("{key_prefix}_{i}");
            let file_stem = file
                .strip_suffix(".json")
                .unwrap_or(file.strip_suffix(".ndjson").unwrap_or(file));
            let json_file = results_dir.join(format!("{key_prefix}-run-{i}-{file_stem}.json"));

            let mut cmds = Vec::new();
            let mut cmd_tools: Vec<&str> = Vec::new();

            if ignore_failure {
                // NDJSON: run all tools (--ignore-failure handles errors)
                for tool in tools {
                    cmds.push(build_cmd(
                        tool,
                        filter.flags,
                        filter.expr,
                        file_path.to_str().unwrap(),
                    ));
                    cmd_tools.push(&tool.name);
                }
            } else {
                // JSON: check support first
                for tool in tools {
                    if tool_supports_filter(tool, filter, &file_path) {
                        cmds.push(build_cmd(
                            tool,
                            filter.flags,
                            filter.expr,
                            file_path.to_str().unwrap(),
                        ));
                        cmd_tools.push(&tool.name);
                    } else {
                        eprintln!("  Skip {} for '{}' (unsupported)", tool.name, filter.name);
                    }
                }
            }

            if cmds.len() < 2 {
                eprintln!(
                    "  Skipping '{}' \u{2014} not enough tools support it",
                    filter.name
                );
                eprintln!();
                continue;
            }

            eprintln!("--- {} ---", filter.name);

            // Build hyperfine command, optionally wrapped with caffeinate on macOS
            let (program, mut prefix_args) = if use_caffeinate && is_macos() {
                ("caffeinate", vec!["-dims", "hyperfine"])
            } else {
                ("hyperfine", vec![])
            };
            prefix_args.extend(["--warmup", "1", "--runs"]);

            let mut hyperfine = Command::new(program);
            for arg in &prefix_args {
                hyperfine.arg(arg);
            }
            hyperfine
                .arg(args.runs.to_string())
                .arg("--export-json")
                .arg(&json_file);
            if ignore_failure {
                hyperfine.arg("--ignore-failure");
            }
            for cmd in &cmds {
                hyperfine.arg(cmd);
            }
            let status = hyperfine.status().expect("failed to run hyperfine");
            if !status.success() {
                eprintln!("  hyperfine failed for '{}'", filter.name);
                continue;
            }
            eprintln!();

            // Parse median times and exit codes from hyperfine JSON output
            let json_content = fs::read_to_string(&json_file).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&json_content).unwrap();
            for (t, tool_name) in cmd_tools.iter().enumerate() {
                if let Some(median) = parsed["results"][t]["median"].as_f64() {
                    let failed = parsed["results"][t]["exit_codes"]
                        .as_array()
                        .is_some_and(|codes| codes.iter().any(|c| c.as_i64() != Some(0)));
                    results.insert(result_key(&filter_key, file, tool_name), (median, failed));
                }
            }

            thread::sleep(Duration::from_secs(args.cooldown));
        }
    }
}

// --- Markdown generation: JSON ---

#[allow(clippy::too_many_arguments)]
fn generate_json_markdown(
    tools: &[Tool],
    json_files: &[&str],
    results: &Results,
    data_dir: &Path,
    runs: u32,
    platform: &str,
    date: &str,
    elapsed: Duration,
) -> String {
    let mut md = String::new();
    writeln!(md, "# Benchmarks").unwrap();
    writeln!(md).unwrap();
    writeln!(
        md,
        "> Auto-generated by `cargo run --release --bin bench_tools -- --type json`. Do not edit manually."
    )
    .unwrap();
    writeln!(
        md,
        "> Last updated: {date} on `{platform}` (total time: {elapsed:.0?})"
    )
    .unwrap();
    writeln!(md).unwrap();
    writeln!(
        md,
        "All benchmarks: warm cache (`--warmup 1`), {runs} runs, output to pipe."
    )
    .unwrap();
    writeln!(
        md,
        "Median of {runs} runs via [hyperfine](https://github.com/sharkdp/hyperfine)."
    )
    .unwrap();
    writeln!(md).unwrap();

    // Build header from available tools (qj variants get a "vs jq" column)
    let mut header = String::from("| Filter | File |");
    let mut separator = String::from("|--------|------|");
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

    // JSON results
    let mut has_failures = false;
    for file in json_files {
        for (i, filter) in JSON_FILTERS.iter().enumerate() {
            let filter_key = format!("json_{i}");
            let display = filter_display(filter);
            let mut row = format!("| `{display}` | {file} |");
            let jq_val = results.get(&result_key(&filter_key, file, "jq"));
            for tool in tools {
                let val = results.get(&result_key(&filter_key, file, &tool.name));
                if val.is_some_and(|v| v.1) {
                    has_failures = true;
                }
                let formatted = format_result(val);
                if tool.name == "qj" {
                    write!(row, " **{formatted}** |").unwrap();
                } else {
                    write!(row, " {formatted} |").unwrap();
                }
                // Add vs jq column for qj variants
                if tool.name.starts_with("qj") {
                    let speedup = match (jq_val, val) {
                        (Some(&(jq_t, _)), Some(&(tool_t, _))) if jq_t > 0.0 && tool_t > 0.0 => {
                            format!("{:.1}x", jq_t / tool_t)
                        }
                        _ => "-".to_string(),
                    };
                    if tool.name == "qj" {
                        write!(row, " **{speedup}** |").unwrap();
                    } else {
                        write!(row, " {speedup} |").unwrap();
                    }
                }
            }
            writeln!(md, "{row}").unwrap();
        }
    }

    // Throughput
    let largest_file = json_files.last().unwrap();
    let largest_path = data_dir.join(largest_file);
    let largest_bytes = fs::metadata(&largest_path).map(|m| m.len()).unwrap_or(0);
    let largest_size_mb = largest_bytes as f64 / (1024.0 * 1024.0);

    let has_throughput = tools
        .iter()
        .any(|tool| results.contains_key(&result_key("json_0", largest_file, &tool.name)));

    if has_throughput {
        writeln!(md).unwrap();
        writeln!(md, "### Throughput").unwrap();
        writeln!(md).unwrap();
        writeln!(
            md,
            "Peak parse throughput (`-c '.'` on {largest_file}, {largest_size_mb:.0}MB):"
        )
        .unwrap();
        writeln!(md).unwrap();

        let mut tp_header = String::from("|");
        let mut tp_sep = String::from("|");
        let mut tp_row = String::from("|");
        let jq_identity = results.get(&result_key("json_0", largest_file, "jq"));
        for tool in tools {
            let val = results.get(&result_key("json_0", largest_file, &tool.name));
            if val.is_some_and(|v| v.1) {
                has_failures = true;
            }
            let tp = format_throughput_result(val, largest_bytes);
            if tool.name == "qj" {
                write!(tp_header, " **{}** |", tool.name).unwrap();
                write!(tp_row, " **{tp}** |").unwrap();
            } else {
                write!(tp_header, " {} |", tool.name).unwrap();
                write!(tp_row, " {tp} |").unwrap();
            }
            write!(tp_sep, "------|").unwrap();
            if tool.name.starts_with("qj") {
                tp_header.push_str(" vs jq |");
                write!(tp_sep, "------|").unwrap();
                let speedup = match (jq_identity, val) {
                    (Some(&(jq_t, _)), Some(&(tool_t, _))) if jq_t > 0.0 && tool_t > 0.0 => {
                        format!("{:.1}x", jq_t / tool_t)
                    }
                    _ => "-".to_string(),
                };
                if tool.name == "qj" {
                    write!(tp_row, " **{speedup}** |").unwrap();
                } else {
                    write!(tp_row, " {speedup} |").unwrap();
                }
            }
        }
        writeln!(md, "{tp_header}").unwrap();
        writeln!(md, "{tp_sep}").unwrap();
        writeln!(md, "{tp_row}").unwrap();
    }

    // Summary: geometric-mean speedup vs jq
    writeln!(md).unwrap();
    writeln!(md, "### Summary (times faster than jq)").unwrap();
    writeln!(md).unwrap();

    let mut sum_header = String::from("|");
    let mut sum_sep = String::from("|");
    for tool in tools {
        if tool.name == "jq" {
            continue;
        }
        if tool.name == "qj" {
            write!(sum_header, " **{}** |", tool.name).unwrap();
        } else {
            write!(sum_header, " {} |", tool.name).unwrap();
        }
        write!(sum_sep, "------|").unwrap();
    }
    writeln!(md, "{sum_header}").unwrap();
    writeln!(md, "{sum_sep}").unwrap();

    // JSON category (use largest file)
    let json_file = json_files.last().unwrap();
    let mut json_row = String::from("|");
    for tool in tools {
        if tool.name == "jq" {
            continue;
        }
        let pairs: Vec<(f64, f64)> = JSON_FILTERS
            .iter()
            .enumerate()
            .filter_map(|(i, _)| {
                let key = format!("json_{i}");
                let jq_val = results.get(&result_key(&key, json_file, "jq"))?.0;
                let tool_val = results.get(&result_key(&key, json_file, &tool.name))?.0;
                Some((jq_val, tool_val))
            })
            .collect();
        let formatted = geomean_ratio(&pairs)
            .map(|v| format!("{v:.1}x"))
            .unwrap_or_else(|| "-".to_string());
        if tool.name == "qj" {
            write!(json_row, " **{formatted}** |").unwrap();
        } else {
            write!(json_row, " {formatted} |").unwrap();
        }
    }
    writeln!(md, "{json_row}").unwrap();

    writeln!(md).unwrap();
    writeln!(
        md,
        "Geometric mean of per-filter speedups (median time). Higher is better."
    )
    .unwrap();

    if has_failures {
        writeln!(md).unwrap();
        writeln!(
            md,
            "\\*non-zero exit code (tool crashed or returned an error)"
        )
        .unwrap();
    }

    md
}

// --- Markdown generation: NDJSON ---

#[allow(clippy::too_many_arguments)]
fn generate_ndjson_markdown(
    tools: &[Tool],
    ndjson_file: &str,
    filters: &[BenchFilter],
    results: &Results,
    data_dir: &Path,
    runs: u32,
    platform: &str,
    date: &str,
    elapsed: Duration,
) -> String {
    let mut md = String::new();
    let file_path = data_dir.join(ndjson_file);
    let file_bytes = fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
    let file_mb = file_bytes / (1024 * 1024);
    let file_size_str = if file_mb >= 1024 {
        format!("{:.1}GB", file_mb as f64 / 1024.0)
    } else {
        format!("{file_mb}MB")
    };

    writeln!(md, "# GH Archive Benchmark").unwrap();
    writeln!(md).unwrap();
    writeln!(
        md,
        "> Generated: {date} on `{platform}` (total time: {elapsed:.0?})"
    )
    .unwrap();
    writeln!(
        md,
        "> {runs} runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine)."
    )
    .unwrap();
    writeln!(md).unwrap();
    writeln!(
        md,
        "### NDJSON ({ndjson_file}, {file_size_str}, parallel processing)"
    )
    .unwrap();
    writeln!(md).unwrap();

    // Build header
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

    let mut has_failures = false;
    for (i, filter) in filters.iter().enumerate() {
        let filter_key = format!("ndjson_{i}");
        let display = filter_display(filter);
        let mut row = format!("| `{display}` |");
        let jq_val = results.get(&result_key(&filter_key, ndjson_file, "jq"));
        for tool in tools {
            let val = results.get(&result_key(&filter_key, ndjson_file, &tool.name));
            if val.is_some_and(|v| v.1) {
                has_failures = true;
            }
            let formatted = format_result(val);
            if tool.name == "qj" {
                write!(row, " **{formatted}** |").unwrap();
            } else {
                write!(row, " {formatted} |").unwrap();
            }
            if tool.name.starts_with("qj") {
                let speedup = match (jq_val, val) {
                    (Some(&(jq_t, _)), Some(&(tool_t, _))) if jq_t > 0.0 && tool_t > 0.0 => {
                        format!("{:.1}x", jq_t / tool_t)
                    }
                    _ => "-".to_string(),
                };
                if tool.name == "qj" {
                    write!(row, " **{speedup}** |").unwrap();
                } else {
                    write!(row, " {speedup} |").unwrap();
                }
            }
        }
        writeln!(md, "{row}").unwrap();
    }

    writeln!(md).unwrap();
    if has_failures {
        writeln!(
            md,
            "\\*non-zero exit code (tool crashed or returned an error)"
        )
        .unwrap();
        writeln!(md).unwrap();
    }

    md
}

fn main() {
    let args = Args::parse();
    let qj_path = "./target/release/qj";
    let data_dir = Path::new("benches/data");
    let results_dir = Path::new("benches/results");

    match args.benchmark_type.as_str() {
        "json" | "ndjson" => {}
        other => {
            eprintln!("Error: unknown --type '{other}'. Use 'json' or 'ndjson'.");
            std::process::exit(1);
        }
    }

    // --- Preflight checks ---
    if !Path::new(qj_path).exists() {
        eprintln!("Error: {qj_path} not found. Run: cargo build --release");
        std::process::exit(1);
    }
    if find_tool("hyperfine").is_none() {
        eprintln!("Error: hyperfine not found.");
        std::process::exit(1);
    }
    fs::create_dir_all(results_dir).unwrap();

    let tools = discover_tools(qj_path);
    let output_path = args.output_path();

    eprintln!(
        "Settings: --type {} --size {} --cooldown {} --runs {} --output {}",
        args.benchmark_type,
        args.size,
        args.cooldown,
        args.runs,
        output_path.display()
    );

    let platform = {
        let os = shell_output("uname", &["-s"]);
        if os.trim().eq_ignore_ascii_case("darwin") {
            let chip = std::process::Command::new("sysctl")
                .args(["-n", "machdep.cpu.brand_string"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "Apple Silicon".to_string());
            let ram = std::process::Command::new("sysctl")
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
    };
    let date = shell_output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]);
    let bench_start = Instant::now();
    eprintln!("Platform: {platform}");
    eprintln!("Date: {date}");
    eprintln!();

    let mut results: Results = HashMap::new();

    if args.benchmark_type == "json" {
        // --- JSON benchmarks ---
        let mut json_files: Vec<&str> = Vec::new();
        if data_dir.join("large_twitter.json").exists() {
            json_files.push("large_twitter.json");
        }
        if json_files.is_empty() {
            eprintln!("Error: no JSON test data found in benches/data/.");
            eprintln!("Run: bash benches/download_testdata.sh && bash benches/gen_large.sh");
            std::process::exit(1);
        }

        // Correctness check
        let qj = &tools[0];
        let jq = &tools[2]; // index 2: jq (after qj and qj 1T)
        if args.skip_correctness {
            eprintln!("=== Correctness check skipped (--skip-correctness) ===");
            eprintln!();
        } else {
            eprintln!("=== Correctness check ===");
            let mut all_correct = true;
            for file in &json_files {
                let file_path = data_dir.join(file);
                for filter in JSON_FILTERS {
                    let qj_out = run_tool_output(qj, filter, &file_path);
                    let jq_out = run_tool_output(jq, filter, &file_path);
                    if qj_out != jq_out {
                        eprintln!("MISMATCH: {} on {file}", filter.name);
                        for (label, out) in [("qj", &qj_out), ("jq", &jq_out)] {
                            let preview: String =
                                out.lines().take(3).collect::<Vec<_>>().join("\n");
                            eprintln!("  {label}: {preview}");
                        }
                        all_correct = false;
                    } else {
                        eprintln!("  OK: {} on {file}", filter.name);
                    }
                }
            }
            eprintln!();
            if !all_correct {
                eprintln!("WARNING: Output mismatches detected. Benchmarking anyway.");
                eprintln!();
            }
        }

        if args.should_run("json") {
            run_benchmarks(
                &tools,
                JSON_FILTERS,
                "json",
                &json_files,
                data_dir,
                results_dir,
                &args,
                &mut results,
                true, // caffeinate on macOS
                false,
            );
        }

        let elapsed = bench_start.elapsed();
        let md = generate_json_markdown(
            &tools,
            &json_files,
            &results,
            data_dir,
            args.runs,
            &platform,
            &date,
            elapsed,
        );
        fs::write(&output_path, &md).unwrap();
    } else {
        // --- NDJSON benchmarks ---
        let (ndjson_file, download_flag) = match args.size.as_str() {
            "small" => ("gharchive.ndjson", ""),
            "medium" => ("gharchive_medium.ndjson", " --medium"),
            "large" => ("gharchive_large.ndjson", " --large"),
            other => {
                eprintln!("Error: unknown --size '{other}'. Use 'small', 'medium', or 'large'.");
                std::process::exit(1);
            }
        };
        let ndjson_path = data_dir.join(ndjson_file);
        if !ndjson_path.exists() {
            eprintln!("Error: {} not found.", ndjson_path.display());
            eprintln!("Run: bash benches/download_gharchive.sh{download_flag}");
            std::process::exit(1);
        }

        let ndjson_filters = match args.size.as_str() {
            "small" => NDJSON_FILTERS,
            _ => NDJSON_FILTERS_SHORT,
        };

        run_benchmarks(
            &tools,
            ndjson_filters,
            "ndjson",
            &[ndjson_file],
            data_dir,
            results_dir,
            &args,
            &mut results,
            true, // caffeinate on macOS
            true, // --ignore-failure
        );

        let elapsed = bench_start.elapsed();
        let md = generate_ndjson_markdown(
            &tools,
            ndjson_file,
            ndjson_filters,
            &results,
            data_dir,
            args.runs,
            &platform,
            &date,
            elapsed,
        );
        fs::write(&output_path, &md).unwrap();
    }

    let total_elapsed = bench_start.elapsed();
    eprintln!("=== Done ({total_elapsed:.0?}) ===");
    eprintln!("Wrote {}", output_path.display());
}
