/// Feature compatibility test runner.
///
/// Parses `tests/jq_compat/features.toml` and runs each test case against
/// qj, jq, jaq, and gojq (whichever are on `$PATH`). Reports a per-feature
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
    #[serde(default)]
    jx_only: bool,
    tests: Vec<TestCase>,
}

#[derive(Deserialize)]
struct TestCase {
    filter: String,
    input: String,
    expected: String,
    flags: Option<String>,
    /// Arguments appended after the filter (for --args/--jsonargs)
    post_args: Option<String>,
    /// If true, compare output as raw bytes (don't filter empty lines)
    raw_compare: Option<bool>,
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
    post_args: Option<&str>,
    cache: &mut common::ToolCache,
) -> Option<String> {
    if post_args.is_some() {
        // Can't use cache for post_args (different invocation shape)
        return run_tool_with_post_args(tool, filter, input, flags, post_args);
    }
    if let Some(flags_str) = flags {
        let parts: Vec<&str> = flags_str.split_whitespace().collect();
        common::run_tool_cached(tool, filter, input, &parts, cache)
    } else {
        common::run_tool_cached(tool, filter, input, &["-c", "--"], cache)
    }
}

fn run_tool_with_post_args(
    tool: &common::Tool,
    filter: &str,
    input: &str,
    flags: Option<&str>,
    post_args: Option<&str>,
) -> Option<String> {
    use std::io::Write;
    let mut cmd = Command::new(&tool.path);
    if let Some(flags_str) = flags {
        cmd.args(flags_str.split_whitespace());
    } else {
        cmd.args(&["-c", "--"]);
    }
    cmd.arg(filter);
    if let Some(pa) = post_args {
        cmd.args(pa.split_whitespace());
    }
    let output = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            child.wait_with_output()
        })
        .ok()?;
    String::from_utf8(output.stdout).ok()
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

/// Hardcoded tests for features that can't be expressed in TOML
/// (binary output, stdin conflicts, etc.)
/// Returns (features, results_per_tool) to be appended to the TOML-driven data.
fn hardcoded_tests(tools: &[common::Tool], verbose: bool) -> (Vec<Feature>, Vec<Vec<Vec<bool>>>) {
    use std::io::Write;

    struct HardcodedTest {
        category: String,
        name: String,
        /// Each sub-test: (description, runner that returns pass/fail per tool)
        cases: Vec<(String, Box<dyn Fn(&common::Tool) -> bool>)>,
    }

    let tests: Vec<HardcodedTest> = vec![
        HardcodedTest {
            category: "CLI flags".into(),
            name: "--raw-output0".into(),
            cases: vec![
                (
                    "NUL-separated string output".into(),
                    Box::new(|tool: &common::Tool| {
                        let mut cmd = Command::new(&tool.path);
                        cmd.args(["--raw-output0", "--", "."]);
                        let output = cmd
                            .stdin(std::process::Stdio::piped())
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .spawn()
                            .and_then(|mut child| {
                                child.stdin.take().unwrap().write_all(b"\"hello\"").unwrap();
                                child.wait_with_output()
                            });
                        match output {
                            Ok(o) => o.stdout == b"hello\0",
                            Err(_) => false,
                        }
                    }),
                ),
                (
                    "NUL-separated array elements".into(),
                    Box::new(|tool: &common::Tool| {
                        let mut cmd = Command::new(&tool.path);
                        cmd.args(["--raw-output0", "--", ".[]"]);
                        let output = cmd
                            .stdin(std::process::Stdio::piped())
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .spawn()
                            .and_then(|mut child| {
                                child
                                    .stdin
                                    .take()
                                    .unwrap()
                                    .write_all(b"[\"a\",\"b\"]")
                                    .unwrap();
                                child.wait_with_output()
                            });
                        match output {
                            Ok(o) => o.stdout == b"a\0b\0",
                            Err(_) => false,
                        }
                    }),
                ),
            ],
        },
        HardcodedTest {
            category: "CLI flags".into(),
            name: "--from-file".into(),
            cases: vec![
                (
                    "Read filter from file".into(),
                    Box::new(|tool: &common::Tool| {
                        // Write filter to a temp file
                        let dir = std::env::temp_dir();
                        let filter_path = dir.join("qj_test_filter.jq");
                        std::fs::write(&filter_path, ".foo").ok();
                        let mut cmd = Command::new(&tool.path);
                        cmd.args(["-f", filter_path.to_str().unwrap(), "--"]);
                        let output = cmd
                            .stdin(std::process::Stdio::piped())
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .spawn()
                            .and_then(|mut child| {
                                child
                                    .stdin
                                    .take()
                                    .unwrap()
                                    .write_all(b"{\"foo\":42}")
                                    .unwrap();
                                child.wait_with_output()
                            });
                        let _ = std::fs::remove_file(&filter_path);
                        match output {
                            Ok(o) => String::from_utf8_lossy(&o.stdout).trim() == "42",
                            Err(_) => false,
                        }
                    }),
                ),
                (
                    "Read complex filter from file".into(),
                    Box::new(|tool: &common::Tool| {
                        let dir = std::env::temp_dir();
                        let filter_path = dir.join("qj_test_filter2.jq");
                        std::fs::write(&filter_path, "[.[] | . * 2]").ok();
                        let mut cmd = Command::new(&tool.path);
                        cmd.args(["-c", "-f", filter_path.to_str().unwrap(), "--"]);
                        let output = cmd
                            .stdin(std::process::Stdio::piped())
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .spawn()
                            .and_then(|mut child| {
                                child.stdin.take().unwrap().write_all(b"[1,2,3]").unwrap();
                                child.wait_with_output()
                            });
                        let _ = std::fs::remove_file(&filter_path);
                        match output {
                            Ok(o) => String::from_utf8_lossy(&o.stdout).trim() == "[2,4,6]",
                            Err(_) => false,
                        }
                    }),
                ),
            ],
        },
    ];

    let mut features = Vec::new();
    let mut all_results: Vec<Vec<Vec<bool>>> = tools.iter().map(|_| Vec::new()).collect();

    for ht in &tests {
        features.push(Feature {
            category: ht.category.clone(),
            name: ht.name.clone(),
            jx_only: false,
            tests: ht
                .cases
                .iter()
                .map(|(desc, _)| TestCase {
                    filter: desc.clone(),
                    input: String::new(),
                    expected: String::new(),
                    flags: None,
                    post_args: None,
                    raw_compare: None,
                })
                .collect(),
        });

        for (ti, tool) in tools.iter().enumerate() {
            let mut case_results = Vec::new();
            for (desc, runner) in &ht.cases {
                let passed = runner(tool);
                if verbose && !passed {
                    eprintln!("  FAIL [{}] {}: {}", tool.name, ht.name, desc);
                }
                case_results.push(passed);
            }
            all_results[ti].push(case_results);
        }
    }

    (features, all_results)
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
    let is_jx = |tool: &common::Tool| tool.name == "qj";

    for tool in &tools {
        let mut tool_results: Vec<Vec<bool>> = Vec::new();
        let mut total_pass = 0usize;
        let mut total_tests = 0usize;

        for feature in &test_file.features {
            let mut feature_results = Vec::new();
            let skip_scoring = feature.jx_only && !is_jx(tool);
            for test in &feature.tests {
                if !skip_scoring {
                    total_tests += 1;
                }
                let output = run_tool_with_flags(
                    tool,
                    &test.filter,
                    &test.input,
                    test.flags.as_deref(),
                    test.post_args.as_deref(),
                    &mut cache,
                );
                let passed = if test.raw_compare.unwrap_or(false) {
                    output.as_deref().map_or(false, |o| o == test.expected)
                } else {
                    test_passes(output.as_deref(), &test.expected)
                };
                if passed && !skip_scoring {
                    total_pass += 1;
                }

                if verbose && !passed && !skip_scoring {
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

    // --- Append hardcoded tests (binary output, stdin conflicts, etc.) ---
    let (extra_features, extra_results) = hardcoded_tests(&tools, verbose);
    let mut all_features: Vec<&Feature> = test_file.features.iter().collect();
    for f in &extra_features {
        all_features.push(f);
    }
    // Merge extra results into main results
    for (ti, tool_extra) in extra_results.into_iter().enumerate() {
        for feature_results in tool_extra {
            results[ti].push(feature_results);
        }
    }

    // Sort features by category to group hardcoded tests with TOML tests.
    // Build index permutation so results arrays stay in sync.
    let mut order: Vec<usize> = (0..all_features.len()).collect();
    order.sort_by_key(|&i| &all_features[i].category);
    let all_features: Vec<&Feature> = order.iter().map(|&i| all_features[i]).collect();
    let results: Vec<Vec<Vec<bool>>> = results
        .into_iter()
        .map(|tool_res| order.iter().map(|&i| tool_res[i].clone()).collect())
        .collect();

    println!();

    // --- Generate markdown ---
    let mut md = String::new();

    md.push_str("## Feature compatibility matrix\n\n");
    md.push_str("Status: **Y** = all tests pass, **~** = partial, **N** = none pass\n\n");

    // Build header/separator (reused for each category)
    let mut header = "| Feature | Tests |".to_string();
    let mut sep = "|---------|------:|".to_string();
    for tool in &tools {
        if tool.name == "qj" {
            header.push_str(" **qj** |");
        } else {
            header.push_str(&format!(" {} |", tool.name));
        }
        sep.push_str("-----:|");
    }

    let mut current_cat = "";

    for (fi, feature) in all_features.iter().enumerate() {
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
            if feature.jx_only && !is_jx(tool) {
                row.push_str(" — |");
                continue;
            }
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
            if is_jx(tool) {
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

    for (ti, tool) in tools.iter().enumerate() {
        let mut y_count = 0usize;
        let mut partial_count = 0usize;
        let mut n_count = 0usize;
        let mut scored_features = 0usize;

        for (fi, feature) in all_features.iter().enumerate() {
            if feature.jx_only && !is_jx(tool) {
                continue;
            }
            scored_features += 1;
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

        let score = if scored_features > 0 {
            (y_count as f64 + 0.5 * partial_count as f64) / scored_features as f64 * 100.0
        } else {
            0.0
        };

        if is_jx(tool) {
            md.push_str(&format!(
                "| **qj** | **{y_count}** | **{partial_count}** | **{n_count}** | **{score:.1}%** |\n"
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
