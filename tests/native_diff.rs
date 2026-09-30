//! Native builtins against their `builtin.jq` definitions, out of process
//! (`#[ignore]`d):
//!
//! ```text
//! cargo test --release --test native_diff -- --ignored --nocapture
//! ```
//!
//! Runs the `qj` binary on generated programs twice, with natives and with
//! `QJ_NO_NATIVE=1` (the bytecode definitions), and compares stdout, stderr and the exit
//! status. The generator is `src/jq/builtins/native/cases.rs` in its unbounded mode: its
//! programs can need unbounded memory or time (in jq too), so every run is capped
//! (`QJ_NATIVE_MEM_MB`, default 512, and 10 s). A case that hits a limit only with
//! natives fails the test (a native must not use more memory than its definition);
//! one that hits it only on the bytecode is skipped (natives are faster).
//!
//! Knobs: `QJ_NATIVE_SEED` (default 1), `QJ_NATIVE_CASES` (default 20000),
//! `QJ_NATIVE_JOBS` (parallel cases, default 6).
//!
//! The in-process test (`native::tests`, in `cargo test`) runs the bounded generator.

#[path = "jq_diff/exec.rs"]
#[allow(dead_code)]
mod exec;

#[path = "../src/jq/builtins/native/cases.rs"]
#[allow(dead_code)]
mod cases;

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use exec::{Merge, Spec, Status};
use rayon::prelude::*;

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// One run of qj: (status, stdout, stderr).
fn run_qj(program: &str, input: &str, natives: bool, max_rss: u64) -> (Status, Vec<u8>, Vec<u8>) {
    let bin = Path::new(env!("CARGO_BIN_EXE_qj"));
    let args = vec!["-c".to_string(), program.to_string()];
    let mut env: Vec<(String, String)> = ["PATH", "HOME"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    if !natives {
        env.push(("QJ_NO_NATIVE".into(), "1".into()));
    }
    let cwd = std::env::temp_dir();
    let spec = Spec {
        bin,
        arg0: None,
        args: &args,
        cwd: &cwd,
        env: &env,
        stdin: Some(input.as_bytes()),
        timeout: Duration::from_secs(10),
        max_output: 1 << 20,
        max_rss,
        merge: Merge::No,
        close_fds: &[],
    };
    match exec::run(&spec) {
        Ok(o) => (o.status, o.stdout, o.stderr),
        Err(e) => panic!("running qj: {e}"),
    }
}

fn limited(s: &Status) -> bool {
    matches!(
        s,
        Status::Timeout | Status::MemoryLimit | Status::OutputLimit
    )
}

#[test]
#[ignore]
fn native_diff() {
    let seed: u64 = env_or("QJ_NATIVE_SEED", 1);
    let n: usize = env_or("QJ_NATIVE_CASES", 20_000);
    let jobs: usize = env_or("QJ_NATIVE_JOBS", 6);
    let max_rss: u64 = env_or::<u64>("QJ_NATIVE_MEM_MB", 512) << 20;

    let mut g = cases::Gen::unbounded(seed);
    let cases: Vec<(String, String)> = (0..n).map(|_| g.case()).collect();

    let failures = Mutex::new(Vec::new());
    let skipped = Mutex::new(0usize);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .unwrap();
    pool.install(|| {
        cases.par_iter().for_each(|(program, input)| {
            let native = run_qj(program, input, true, max_rss);
            let bytecode = run_qj(program, input, false, max_rss);
            if limited(&bytecode.0) {
                *skipped.lock().unwrap() += 1;
                return;
            }
            if native != bytecode {
                let show = |r: &(Status, Vec<u8>, Vec<u8>)| {
                    format!(
                        "status {}\n  stdout: {:?}\n  stderr: {:?}",
                        r.0,
                        String::from_utf8_lossy(&r.1),
                        String::from_utf8_lossy(&r.2)
                    )
                };
                failures.lock().unwrap().push(format!(
                    "program: {program}\ninput: {input}\nnative: {}\nbytecode: {}",
                    show(&native),
                    show(&bytecode)
                ));
            }
        });
    });
    let failures = failures.into_inner().unwrap();
    let skipped = skipped.into_inner().unwrap();
    eprintln!(
        "native_diff: {n} cases (seed {seed}), {} differ, {skipped} skipped at a limit",
        failures.len()
    );
    for f in failures.iter().take(20) {
        eprintln!("---\n{f}");
    }
    assert!(failures.is_empty(), "{} cases differ", failures.len());
}
