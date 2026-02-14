/// Feature compatibility test runner.
///
/// Parses `tests/jq_compat/features.toml` and runs each test case against
/// jx, jq, jaq, and gojq (whichever are on `$PATH`). Reports a per-feature
/// Y/~/N matrix and overall compatibility scores.
///
/// This test always passes — it's a measurement tool, not a gate.
/// Run with `--nocapture` to see the summary:
///
///   cargo test feature_compat -- --nocapture
///
/// To see each failing test case, run the ignored verbose test:
///
///   cargo test feature_compat_verbose -- --nocapture --ignored
mod common;

use serde::Deserialize;
use std::process::Command;

extern crate serde_json;

#[derive(Deserialize)]
struct TestFile {
    features: Vec<Feature>,
}

#[derive(Deserialize)]
struct Feature {
    category: String,
    name: String,
    tests: Vec<TestCase>,
}

#[derive(Deserialize)]
struct TestCase {
    filter: String,
    input: String,
    expected: String,
    flags: Option<String>,
}

fn load_test_file() -> TestFile {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/jq_compat/features.toml");
    let content = std::fs::read_to_string(&path).expect("failed to read features.toml");
    toml::from_str(&content).expect("failed to parse features.toml")
}

fn run_tool_with_flags(
    tool: &common::Tool,
    filter: &str,
    input: &str,
    flags: Option<&str>,
    cache: &mut common::ToolCache,
) -> Option<String> {
    if let Some(flags_str) = flags {
        let parts: Vec<&str> = flags_str.split_whitespace().collect();
        common::run_tool_cached(tool, filter, input, &parts, cache)
    } else {
        common::run_tool_cached(tool, filter, input, &["-c", "--"], cache)
    }
}

fn test_passes(output: Option<&str>, expected: &str) -> bool {
    let output = match output {
        Some(s) => s,
        None => {
            // Tool failed to run; pass only if expected is empty
            return expected.lines().all(|l| l.is_empty());
        }
    };

    let actual_lines: Vec<&str> = output.lines().filter(|l| !l.is_empty()).collect();
    let expected_lines: Vec<&str> = expected.lines().filter(|l| !l.is_empty()).collect();

    // Empty expected matches empty actual
    if expected_lines.is_empty() {
        return actual_lines.is_empty();
    }

    // Non-empty expected requires non-empty actual
    if actual_lines.is_empty() {
        return false;
    }

    common::json_lines_equal(&actual_lines, &expected_lines)
}

fn run_all(verbose: bool) {
    let test_file = load_test_file();
    let tools = common::discover_tools();

    let features_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/jq_compat/features.toml");
    let features_content =
        std::fs::read_to_string(&features_path).expect("failed to read features.toml");
    let content_hash = common::compute_cache_hash(&features_content);
    let loaded = common::load_cache("feature_compat", content_hash);
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

    // results[tool_idx][feature_idx] = Vec<bool> (per-test pass/fail)
    let mut results: Vec<Vec<Vec<bool>>> = Vec::new();

    println!("Running tests...");
    for tool in &tools {
        let mut tool_results: Vec<Vec<bool>> = Vec::new();
        let mut total_pass = 0usize;
        let mut total_tests = 0usize;

        for feature in &test_file.features {
            let mut feature_results = Vec::new();
            for test in &feature.tests {
                total_tests += 1;
                let output = run_tool_with_flags(
                    tool,
                    &test.filter,
                    &test.input,
                    test.flags.as_deref(),
                    &mut cache,
                );
                let passed = test_passes(output.as_deref(), &test.expected);
                if passed {
                    total_pass += 1;
                }

                if verbose && !passed {
                    eprintln!(
                        "  FAIL [{}] {}: {} | input: {}",
                        tool.name, feature.name, test.filter, test.input
                    );
                    let expected_preview: String = test
                        .expected
                        .lines()
                        .take(3)
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

                feature_results.push(passed);
            }
            tool_results.push(feature_results);
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

        results.push(tool_results);
    }
    common::save_cache("feature_compat", &cache);
    println!();

    // --- Generate markdown ---
    let mut md = String::new();

    md.push_str("## Feature compatibility matrix\n\n");
    md.push_str("Status: **Y** = all tests pass, **~** = partial, **N** = none pass\n\n");

    // Build header/separator (reused for each category)
    let mut header = "| Feature | Tests |".to_string();
    let mut sep = "|---------|------:|".to_string();
    for tool in &tools {
        if tool.name == "jx" {
            header.push_str(" **jx** |");
        } else {
            header.push_str(&format!(" {} |", tool.name));
        }
        sep.push_str("-----:|");
    }

    let mut current_cat = "";

    for (fi, feature) in test_file.features.iter().enumerate() {
        if feature.category != current_cat {
            if !current_cat.is_empty() {
                md.push('\n'); // blank line before new category header
            }
            current_cat = &feature.category;
            md.push_str(&format!("### {}\n\n", feature.category));
            md.push_str(&format!("{header}\n{sep}\n"));
        }

        let test_count = feature.tests.len();
        let mut row = format!("| {} | {} |", feature.name, test_count);

        for (ti, tool) in tools.iter().enumerate() {
            let pass_count = results[ti][fi].iter().filter(|&&p| p).count();
            let total = results[ti][fi].len();
            let status = if pass_count == total && total > 0 {
                "Y"
            } else if pass_count > 0 {
                "~"
            } else {
                "N"
            };
            let cell = format!("{pass_count}/{total} {status}");
            if tool.name == "jx" {
                row.push_str(&format!(" **{cell}** |"));
            } else {
                row.push_str(&format!(" {cell} |"));
            }
        }

        md.push_str(&format!("{row}\n"));
    }

    md.push('\n');

    // --- Summary table ---
    md.push_str("## Summary\n\n");
    md.push_str("| Tool | Y | ~ | N | Score |\n");
    md.push_str("|------|--:|--:|--:|------:|\n");

    let feature_count = test_file.features.len();

    for (ti, tool) in tools.iter().enumerate() {
        let mut y_count = 0usize;
        let mut partial_count = 0usize;
        let mut n_count = 0usize;

        for (fi, feature) in test_file.features.iter().enumerate() {
            let pass_count = results[ti][fi].iter().filter(|&&p| p).count();
            let total = feature.tests.len();
            if pass_count == total && total > 0 {
                y_count += 1;
            } else if pass_count > 0 {
                partial_count += 1;
            } else {
                n_count += 1;
            }
        }

        let score = (y_count as f64 + 0.5 * partial_count as f64) / feature_count as f64 * 100.0;

        if tool.name == "jx" {
            md.push_str(&format!(
                "| **jx** | **{y_count}** | **{partial_count}** | **{n_count}** | **{score:.1}%** |\n"
            ));
        } else {
            md.push_str(&format!(
                "| {} | {} | {} | {} | {:.1}% |\n",
                tool.name, y_count, partial_count, n_count, score
            ));
        }
    }

    md.push('\n');
    md.push_str("Score = (Y + 0.5 \u{00d7} ~) / total \u{00d7} 100\n");

    println!("{md}");

    // Write results file
    let results_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/jq_compat/feature_results.md");

    let date = Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let date = date.trim();

    let mut file_content = String::new();
    file_content.push_str("# Feature compatibility results\n\n");
    file_content.push_str(&format!("Generated by `feature_compat` on {date}.\n\n"));
    file_content.push_str(&md);

    std::fs::write(&results_path, file_content).expect("failed to write feature_results.md");
    println!("Results written to {}", results_path.display());
}

/// Run with: cargo test --release feature_compat -- --nocapture --ignored
#[test]
#[ignore]
fn feature_compat() {
    run_all(false);
}

/// Run with: cargo test feature_compat_verbose -- --nocapture --ignored
#[test]
#[ignore]
fn feature_compat_verbose() {
    run_all(true);
}
