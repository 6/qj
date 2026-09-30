//! The VM's optimized code (regions and frameless calls, `src/jq/lang/execute/region.rs`)
//! against jq's instructions as they are, out of process (`#[ignore]`d):
//!
//! ```text
//! cargo test --release --test vm_opt_diff -- --ignored --nocapture
//! ```
//!
//! Runs the `qj` binary on generated programs twice, optimized and with
//! `QJ_NO_VM_OPT=1`, and compares stdout, stderr and the exit status; half the cases
//! also run with `QJ_NO_NATIVE=1` (both times), so regions are checked both inside
//! natives' closures and on jq's definitions. The generator is
//! `src/jq/lang/execute/opt_cases.rs` in its unbounded mode: its programs can need
//! unbounded memory or time (in jq too), so every run is capped (`QJ_OPT_MEM_MB`,
//! default 1024, and 10 s). A case that hits a limit only when optimized fails the
//! test; one that hits it only unoptimized is skipped.
//!
//! Knobs: `QJ_OPT_SEED` (default 1), `QJ_OPT_CASES` (default 20000), `QJ_OPT_JOBS`
//! (parallel cases, default 6).
//!
//! The in-process test (`execute::opt_tests`, in `cargo test`) runs the bounded generator.

#[path = "jq_diff/exec.rs"]
#[allow(dead_code)]
mod exec;

#[path = "../src/jq/lang/execute/opt_cases.rs"]
#[allow(dead_code)]
mod opt_cases;

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
fn run_qj(
    program: &str,
    input: &str,
    optimized: bool,
    natives: bool,
    max_rss: u64,
) -> (Status, Vec<u8>, Vec<u8>) {
    let bin = Path::new(env!("CARGO_BIN_EXE_qj"));
    let args = vec!["-c".to_string(), program.to_string()];
    let mut env: Vec<(String, String)> = ["PATH", "HOME"]
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    if !optimized {
        env.push(("QJ_NO_VM_OPT".into(), "1".into()));
    }
    if !natives {
        env.push(("QJ_NO_NATIVE".into(), "1".into()));
    }
    let cwd = std::env::temp_dir();
    let spec = Spec {
        bin,
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
fn vm_opt_diff() {
    let seed: u64 = env_or("QJ_OPT_SEED", 1);
    let n: usize = env_or("QJ_OPT_CASES", 20_000);
    let jobs: usize = env_or("QJ_OPT_JOBS", 6);
    let max_rss: u64 = env_or::<u64>("QJ_OPT_MEM_MB", 1024) << 20;

    let mut g = opt_cases::Gen::unbounded(seed);
    let cases: Vec<(String, String)> = (0..n).map(|_| g.case()).collect();

    let failures = Mutex::new(Vec::new());
    let skipped = Mutex::new(0usize);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .unwrap();
    pool.install(|| {
        cases
            .par_iter()
            .enumerate()
            .for_each(|(i, (program, input))| {
                let natives = i % 2 == 0;
                let opt = run_qj(program, input, true, natives, max_rss);
                let orig = run_qj(program, input, false, natives, max_rss);
                if limited(&orig.0) {
                    *skipped.lock().unwrap() += 1;
                    return;
                }
                if opt != orig {
                    let show = |r: &(Status, Vec<u8>, Vec<u8>)| {
                        format!(
                            "status {}\n  stdout: {:?}\n  stderr: {:?}",
                            r.0,
                            String::from_utf8_lossy(&r.1),
                            String::from_utf8_lossy(&r.2)
                        )
                    };
                    failures.lock().unwrap().push(format!(
                        "program: {program}\ninput: {input}\nnatives: {natives}\noptimized: {}\noriginal: {}",
                        show(&opt),
                        show(&orig)
                    ));
                }
            });
    });
    let failures = failures.into_inner().unwrap();
    let skipped = skipped.into_inner().unwrap();
    eprintln!(
        "vm_opt_diff: {n} cases (seed {seed}), {} differ, {skipped} skipped at a limit",
        failures.len()
    );
    for f in failures.iter().take(20) {
        eprintln!("---\n{f}");
    }
    assert!(failures.is_empty(), "{} cases differ", failures.len());
}
