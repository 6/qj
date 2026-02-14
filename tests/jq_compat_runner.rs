/// Cross-tool jq compatibility runner.
///
/// Parses the official jq test suite (`tests/jq_compat/jq.test` from jqlang/jq)
/// and runs each test case against jx, jq, jaq, and gojq (whichever are on
/// `$PATH`). Reports per-tool pass rates with a per-category breakdown.
/// Results are written to `tests/jq_compat/results.md`.
///
/// This test always passes — it's a measurement tool, not a gate.
/// Run with `--nocapture` to see the summary:
///
///   cargo test jq_compat -- --nocapture
///
/// To see each failing test case, run the ignored verbose test:
///
///   cargo test jq_compat_verbose -- --nocapture --ignored
mod common;

use std::collections::BTreeMap;
use std::process::Command;

extern crate serde_json;

struct TestCase {
    filter: String,
    input: String,
    expected: Vec<String>,
    category: String,
    line_no: usize,
}

/// Section headers in jq.test — match prefix is checked against comment text,
/// display name is used in output. Mirrors the bash script's section_headers array.
const SECTION_HEADERS: &[(&str, &str)] = &[
    ("Simple value tests", "Simple value tests"),
    (
        "Dictionary construction syntax",
        "Dictionary construction syntax",
    ),
    ("Field access, piping", "Field access, piping"),
    ("Negative array indices", "Negative array indices"),
    ("Multiple outputs, iteration", "Multiple outputs, iteration"),
    ("Slices", "Slices"),
    ("Variables", "Variables"),
    ("Builtin functions", "Builtin functions"),
    ("User-defined functions", "User-defined functions"),
    ("Paths", "Paths"),
    ("Assignment", "Assignment"),
    ("Conditionals", "Conditionals"),
    ("string operations", "string operations"),
    ("module system", "module system"),
    ("Basic numbers tests", "Basic numbers tests"),
    (
        "Tests to cover the new toliteral number",
        "toliteral number",
    ),
    ("explode/implode", "explode/implode"),
    ("walk", "walk"),
];

fn match_section_header(comment: &str) -> Option<&'static str> {
    for &(prefix, display) in SECTION_HEADERS {
        if comment.starts_with(prefix) {
            return Some(display);
        }
    }
    None
}

fn parse_jq_test_file(content: &str) -> Vec<TestCase> {
    let mut cases = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    let mut current_category = "Other".to_string();

    while i < lines.len() {
        let line = lines[i];

        // Skip blank lines
        if line.trim().is_empty() {
            i += 1;
            continue;
        }

        // Comments: check for section header
        if line.starts_with('#') {
            if let Some(rest) = line.strip_prefix("# ") {
                if let Some(display) = match_section_header(rest) {
                    current_category = display.to_string();
                }
            }
            i += 1;
            continue;
        }

        // Skip %%FAIL blocks
        if line.starts_with("%%FAIL") {
            i += 1;
            while i < lines.len() && !lines[i].trim().is_empty() {
                i += 1;
            }
            continue;
        }

        // Filter line
        let filter_line = i + 1; // 1-indexed
        let filter = line.to_string();
        i += 1;

        // Input line
        if i >= lines.len() {
            break;
        }
        let input = lines[i].to_string();
        i += 1;

        // Expected output lines until blank line, comment, or EOF
        let mut expected = Vec::new();
        while i < lines.len() && !lines[i].trim().is_empty() && !lines[i].starts_with('#') {
            expected.push(lines[i].to_string());
            i += 1;
        }

        if !expected.is_empty() {
            cases.push(TestCase {
                filter,
                input,
                expected,
                category: current_category.clone(),
                line_no: filter_line,
            });
        }
    }

    cases
}

/// Extra args per tool: jq/jaq/gojq get `-L modules` for module system tests.
fn extra_args_for(tool: &common::Tool) -> Vec<String> {
    let modules_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/jq_compat/modules")
        .to_string_lossy()
        .to_string();
    match tool.name.as_str() {
        "jq" | "jaq" | "gojq" => vec![
            "-L".to_string(),
            modules_dir,
            "-c".to_string(),
            "--".to_string(),
        ],
        _ => vec!["-c".to_string(), "--".to_string()],
    }
}

fn run_all(verbose: bool) {
    let test_file =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/jq_compat/jq.test");
    let content = std::fs::read_to_string(&test_file).expect("failed to read jq.test");
    let cases = parse_jq_test_file(&content);
    let tools = common::discover_tools();

    let content_hash = common::compute_cache_hash(&content);
    let loaded = common::load_cache("jq_compat_runner", content_hash);
    let cache_hit = loaded.is_some();
    let mut cache = loaded.unwrap_or(common::ToolCache {
        content_hash,
        results: std::collections::HashMap::new(),
    });

    println!();
    println!(
        "Tools: {}",
        tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if cache_hit {
        println!("  (using cached results for external tools)");
    }
    println!();

    // Collect ordered categories
    let mut categories: Vec<String> = Vec::new();
    for case in &cases {
        if !categories.contains(&case.category) {
            categories.push(case.category.clone());
        }
    }

    // Per-tool, per-category: (passed, total)
    // tool_name -> category -> (passed, total)
    let mut cat_stats: BTreeMap<String, BTreeMap<String, (usize, usize)>> = BTreeMap::new();

    println!("jq compat (jq.test):");
    for tool in &tools {
        let extra = extra_args_for(tool);
        let extra_refs: Vec<&str> = extra.iter().map(|s| s.as_str()).collect();

        let mut total_pass = 0usize;
        let mut total_tests = 0usize;

        let tool_cats = cat_stats
            .entry(tool.name.clone())
            .or_insert_with(BTreeMap::new);

        for case in &cases {
            total_tests += 1;
            let output =
                common::run_tool_cached(tool, &case.filter, &case.input, &extra_refs, &mut cache);

            let actual_lines: Vec<&str> = output
                .as_deref()
                .unwrap_or("")
                .lines()
                .filter(|l| !l.is_empty())
                .collect();
            let expected_lines: Vec<&str> = case.expected.iter().map(|s| s.as_str()).collect();

            let passed = if output.is_some() {
                common::json_lines_equal(&actual_lines, &expected_lines)
            } else {
                // Tool couldn't run; pass only if no expected output
                expected_lines.is_empty()
            };

            let entry = tool_cats.entry(case.category.clone()).or_insert((0, 0));
            entry.1 += 1;
            if passed {
                entry.0 += 1;
                total_pass += 1;
            }

            if verbose && !passed {
                eprintln!(
                    "  FAIL [{}] (line {}): {} | input: {}",
                    tool.name, case.line_no, case.filter, case.input
                );
                let expected_preview: String = case
                    .expected
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" | ");
                eprintln!("    expected: {expected_preview}");
                if let Some(actual) = &output {
                    let actual_preview: String =
                        actual.lines().take(3).collect::<Vec<_>>().join(" | ");
                    eprintln!("    actual:   {actual_preview}");
                } else {
                    eprintln!("    actual:   <error>");
                }
            }
        }

        let pct = if total_tests > 0 {
            total_pass as f64 / total_tests as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "  {:<6} {:>3}/{} passed ({:>5.1}%)",
            tool.name, total_pass, total_tests, pct
        );
    }
    common::save_cache("jq_compat_runner", &cache);
    println!();

    // --- Generate markdown ---
    let mut md = String::new();

    md.push_str("# jq compatibility results\n\n");

    let date = Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let date = date.trim();
    md.push_str(&format!("Generated by `jq_compat_runner` on {date}.\n\n"));

    // Summary bullets
    md.push_str("## Summary\n\n");
    for tool in &tools {
        let stats = cat_stats.get(&tool.name).unwrap();
        let total_pass: usize = stats.values().map(|(p, _)| p).sum();
        let total_tests: usize = stats.values().map(|(_, t)| t).sum();
        let pct = if total_tests > 0 {
            total_pass as f64 / total_tests as f64 * 100.0
        } else {
            0.0
        };
        md.push_str(&format!(
            "- **{}**: {}/{} ({:.1}%)\n",
            tool.name, total_pass, total_tests, pct
        ));
    }
    md.push('\n');

    // Per-category table
    md.push_str("## Per-category breakdown\n\n");

    // Header
    let mut header = "| Category".to_string();
    for _ in " ".repeat(33 - "Category".len()).chars() {
        header.push(' ');
    }
    header.truncate(0);
    header.push_str("| Category                         |");
    for tool in &tools {
        header.push_str(&format!(" {:<10} |", tool.name));
    }
    md.push_str(&format!("{header}\n"));

    // Separator
    let mut sep = "|".to_string();
    sep.push_str("----------------------------------|");
    for _ in &tools {
        sep.push_str("------------|");
    }
    md.push_str(&format!("{sep}\n"));

    // Data rows
    let mut grand_pass: BTreeMap<String, usize> = BTreeMap::new();
    let mut grand_total: BTreeMap<String, usize> = BTreeMap::new();

    for cat in &categories {
        let mut row = format!("| {:<32} |", cat);
        for tool in &tools {
            let (p, t) = cat_stats
                .get(&tool.name)
                .and_then(|m| m.get(cat))
                .copied()
                .unwrap_or((0, 0));
            *grand_pass.entry(tool.name.clone()).or_default() += p;
            *grand_total.entry(tool.name.clone()).or_default() += t;
            if t == 0 {
                row.push_str(&format!(" {:<10} |", "-"));
            } else {
                row.push_str(&format!(" {:<10} |", format!("{p}/{t}")));
            }
        }
        md.push_str(&format!("{row}\n"));
    }

    // Total row (bold)
    let mut row = format!("| {:<32} |", "**Total**");
    for tool in &tools {
        let p = grand_pass.get(&tool.name).copied().unwrap_or(0);
        let t = grand_total.get(&tool.name).copied().unwrap_or(0);
        row.push_str(&format!(" {:<10} |", format!("**{p}/{t}**")));
    }
    md.push_str(&format!("{row}\n"));

    println!("Per-category breakdown:\n");
    // Print table portion for console
    let table_start = md.find("| Category").unwrap_or(0);
    println!("{}", &md[table_start..]);

    // Write results file
    let results_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/jq_compat/results.md");
    std::fs::write(&results_path, &md).expect("failed to write results.md");
    println!("Results written to {}", results_path.display());
}

/// Run with: cargo test --release jq_compat -- --nocapture --ignored
#[test]
#[ignore]
fn jq_compat() {
    run_all(false);
}

/// Run with: cargo test jq_compat_verbose -- --nocapture --ignored
#[test]
#[ignore]
fn jq_compat_verbose() {
    run_all(true);
}
