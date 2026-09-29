//! Differential test: the fast reader (simdjson fast path, streams, chunk
//! emulation) against the line-by-line port of jq's `util.c`
//! ([`super::reference`]) over generated adversarial inputs.

use super::generate as gen_input;
use super::reference::RefInput;
use super::{Delivery, Ev, Rng, events, mem_reader};
use crate::io::reader::ReaderOptions;
use crate::jq::value::ParseFlags;

/// `next()` calls per case (the reader keeps going after errors, as
/// `try input` does).
const LIMIT: usize = 400;

fn ref_events(r: &mut RefInput, limit: usize) -> Vec<Ev> {
    let mut evs = Vec::new();
    for _ in 0..limit {
        let next = r.next_input();
        let m = std::mem::take(&mut r.messages);
        if !m.is_empty() {
            evs.push(Ev::Message(m));
        }
        let (f, l, n) = (r.filename_value(), r.current_line, r.failures);
        match next {
            Some(Ok(v)) => evs.push(Ev::Value(v, f, l, n)),
            Some(Err(e)) => evs.push(Ev::Error(e.to_string(), f, l, n)),
            None => {
                evs.push(Ev::End(f, l, n));
                break;
            }
        }
    }
    evs
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Json,
    Slurp,
    Raw,
    RawSlurp,
    Seq,
    Stream,
    StreamErrors,
}

fn options(mode: Mode) -> ReaderOptions {
    let mut o = ReaderOptions::default();
    match mode {
        Mode::Json => {}
        Mode::Slurp => o.slurp = true,
        Mode::Raw => o.raw = true,
        Mode::RawSlurp => {
            o.raw = true;
            o.slurp = true;
        }
        Mode::Seq => o.seq = true,
        Mode::Stream => o.stream = true,
        Mode::StreamErrors => o.stream_errors = true,
    }
    o
}

fn pick_mode(r: &mut Rng) -> Mode {
    match r.below(20) {
        0 | 1 => Mode::Slurp,
        2 | 3 => Mode::Raw,
        4 => Mode::RawSlurp,
        5 => Mode::Seq,
        6 => Mode::Stream,
        7 => Mode::StreamErrors,
        _ => Mode::Json,
    }
}

fn describe(evs: &[Ev]) -> String {
    evs.iter()
        .map(|e| match e {
            Ev::Value(v, f, l, n) => format!("value {} @{f:?}:{l} fail={n}", trunc(&v.to_json())),
            Ev::Error(m, f, l, n) => format!("error {m:?} @{f:?}:{l} fail={n}"),
            Ev::End(f, l, n) => format!("end @{f:?}:{l} fail={n}"),
            Ev::Message(m) => format!("message {:?}", String::from_utf8_lossy(m)),
        })
        .collect::<Vec<_>>()
        .join("\n    ")
}

fn trunc(s: &str) -> String {
    if s.len() > 120 {
        let mut end = 120;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    } else {
        s.to_owned()
    }
}

/// Runs one generated case; `Err` describes a divergence.
fn run_case(seed: u64) -> Result<(), String> {
    let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    let mode = pick_mode(&mut rng);
    let data = gen_input::stream(&mut rng);
    let (names, files) = gen_input::inputs(&mut rng, &data);
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let opts = options(mode);
    let flags = ParseFlags {
        seq: opts.seq,
        streaming: opts.stream || opts.stream_errors,
        stream_errors: opts.stream_errors,
    };
    let mut reference = RefInput::new(&names, files.clone(), opts.raw, opts.slurp, flags);
    let want = ref_events(&mut reference, LIMIT);
    let delivery = match rng.below(4) {
        0 => Delivery::Whole,
        1 => Delivery::Stream {
            seed: rng.next(),
            max: 1 + rng.below(8),
        },
        2 => Delivery::Stream {
            seed: rng.next(),
            max: 1 + rng.below(5000),
        },
        _ => Delivery::Whole,
    };
    for fast in [true, false] {
        let (mut r, msgs) = mem_reader(&names, files.clone(), opts, delivery, fast);
        let got = events(&mut r, &msgs, LIMIT);
        if got != want {
            let first = got
                .iter()
                .zip(want.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(got.len().min(want.len()));
            return Err(format!(
                "seed {seed} {mode:?} {delivery:?} fast={fast} names={names:?}\n  input: {:?}\n  first difference at event {first}\n  got:\n    {}\n  want:\n    {}",
                trunc(&String::from_utf8_lossy(&data)),
                describe(&got[first.saturating_sub(2)..(first + 3).min(got.len())]),
                describe(&want[first.saturating_sub(2)..(first + 3).min(want.len())]),
            ));
        }
    }
    Ok(())
}

fn run(seeds: std::ops::Range<u64>) {
    let mut failures = Vec::new();
    let n = seeds.end - seeds.start;
    for seed in seeds {
        if let Err(e) = run_case(seed) {
            failures.push(e);
            if failures.len() >= 5 {
                break;
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {n} cases diverge from jq's util.c:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn reader_matches_util_c() {
    run(0..600);
}

/// The long run: `cargo test --release --lib reader_matches_util_c_long -- --ignored`
#[test]
#[ignore]
fn reader_matches_util_c_long() {
    let n: u64 = std::env::var("QJ_IO_DIFF_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000);
    run(1_000_000..1_000_000 + n);
}
