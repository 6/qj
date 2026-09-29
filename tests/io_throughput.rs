//! Sanity timings for the src/io input layer, with the binary's allocator.
//! `#[ignore]`d: these are not benchmarks (the numbers are noisy), just a
//! quick way to see where time goes:
//! `cargo test --release --test io_throughput -- --ignored --nocapture`

use mimalloc::MiMalloc;
use std::time::Instant;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use std::ops::ControlFlow;

use qj::io::parallel::{self, DumpFactory, EngineOptions, RecordSink};
use qj::io::simd::SimdParser;
use qj::io::{InputReader, MemoryOpener, ReaderOptions};
use qj::jq::value::{DumpOptions, ParseFlags, Parser};

fn synthetic_ndjson(records: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut x: u64 = 0x2545F4914F6CDD1D;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for i in 0..records {
        let r = rnd();
        let line = format!(
            r#"{{"id":"{}","type":"PushEvent","actor":{{"id":{},"login":"user{}","display_login":"user{}","gravatar_id":"","url":"https://api.github.com/users/user{}","avatar_url":"https://avatars.githubusercontent.com/u/{}?"}},"repo":{{"id":{},"name":"org{}/repo{}","url":"https://api.github.com/repos/org/repo"}},"payload":{{"push_id":{},"size":{},"distinct_size":1,"ref":"refs/heads/main","head":"{:016x}{:016x}","commits":[{{"sha":"{:016x}","author":{{"email":"a{}@example.com","name":"Author {}"}},"message":"Fix the thing\nwith a newline and \"quotes\" and unicode é","distinct":true,"url":"https://api.github.com/repos/x/y/commits/abc"}}]}},"public":{},"created_at":"2024-01-01T00:00:{:02}Z","score":{}.{:02}}}"#,
            20000000000u64 + i as u64,
            r % 100000000,
            r % 1000,
            r % 1000,
            r % 1000,
            r % 100000,
            r % 1000000,
            r % 50,
            r % 70,
            r % 10000000000,
            r % 20,
            r,
            r.rotate_left(17),
            r.rotate_left(29),
            r % 997,
            r % 991,
            r % 2 == 0,
            i % 60,
            r % 100,
            r % 100,
        );
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    out
}

#[test]
#[ignore]
fn parse_throughput_sanity() {
    let data = synthetic_ndjson(100_000);
    let mb = data.len() as f64 / 1e6;
    for round in 0..2 {
        let t = Instant::now();
        let mut p = Parser::new(ParseFlags::default());
        p.set_buf(&data, false);
        let mut n = 0;
        while let Some(Ok(_)) = p.next() {
            n += 1;
        }
        let dt = t.elapsed().as_secs_f64();
        eprintln!("[{round}] jq parser port: {n} values, {:.0} MB/s", mb / dt);

        let t = Instant::now();
        let mut s = SimdParser::new();
        let mut n = 0;
        let mut start = 0;
        for nl in memchr::memchr_iter(b'\n', &data) {
            let _v = s.parse(&data, start, nl).unwrap();
            n += 1;
            start = nl + 1;
        }
        let dt = t.elapsed().as_secs_f64();
        eprintln!(
            "[{round}] simdjson -> Value: {n} values, {:.0} MB/s",
            mb / dt
        );

        // The reader over the same bytes as one input (NDJSON fast path).
        let t = Instant::now();
        let mut m = MemoryOpener::new();
        m.add("f", data.clone());
        let mut r =
            InputReader::with_opener(vec!["f".into()], ReaderOptions::default(), Box::new(m));
        let mut n = 0;
        while let Some(v) = r.next() {
            v.unwrap();
            n += 1;
        }
        let dt = t.elapsed().as_secs_f64();
        eprintln!("[{round}] reader (NDJSON): {n} values, {:.0} MB/s", mb / dt);

        // The same records pretty-printed inside one array.
        let mut pretty = b"[\n".to_vec();
        for (i, line) in data
            .split(|&c| c == b'\n')
            .filter(|l| !l.is_empty())
            .enumerate()
        {
            if i > 0 {
                pretty.extend_from_slice(b",\n");
            }
            let v = qj::jq::value::parse_sized(line).unwrap();
            pretty.extend_from_slice(
                qj::jq::value::dump_string(&v, &qj::jq::value::DumpOptions::pretty()).as_bytes(),
            );
        }
        pretty.extend_from_slice(b"\n]\n");
        let pmb = pretty.len() as f64 / 1e6;
        let t = Instant::now();
        let mut m = MemoryOpener::new();
        m.add("f", pretty.clone());
        let mut r =
            InputReader::with_opener(vec!["f".into()], ReaderOptions::default(), Box::new(m));
        let v = r.next().unwrap().unwrap();
        assert!(r.next().is_none());
        let dt = t.elapsed().as_secs_f64();
        eprintln!(
            "[{round}] reader (one pretty-printed {pmb:.0} MB array of {}): {:.0} MB/s",
            v.as_array().unwrap().len(),
            pmb / dt
        );
        drop(v);

        // Parse + print, sequential vs the engine.
        for threads in [0, 4, 16] {
            let t = Instant::now();
            let mut m = MemoryOpener::new();
            m.add("f", data.clone());
            let mut r =
                InputReader::with_opener(vec!["f".into()], ReaderOptions::default(), Box::new(m));
            let factory = DumpFactory {
                opts: DumpOptions::compact(),
                with_position: false,
            };
            let mut sink = Count(0);
            let opts = EngineOptions {
                threads,
                ..EngineOptions::default()
            };
            let stats = parallel::run(&mut r, &factory, &mut sink, &opts);
            let dt = t.elapsed().as_secs_f64();
            eprintln!(
                "[{round}] parse+print, {threads} threads: {} bytes out, {:.0} MB/s ({} by workers)",
                sink.0,
                mb / dt,
                stats.worker_records
            );
        }
    }
}

/// A loop to attach a profiler to (`QJ_PROFILE_SECS`, default 8):
/// `cargo test --profile profiling --test io_throughput profile_loop -- --ignored`
#[test]
#[ignore]
fn profile_loop() {
    let secs: u64 = std::env::var("QJ_PROFILE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    let data = synthetic_ndjson(20_000);
    let mut s = SimdParser::new();
    let t = Instant::now();
    let mut n = 0u64;
    while t.elapsed().as_secs() < secs {
        let mut start = 0;
        for nl in memchr::memchr_iter(b'\n', &data) {
            let _v = s.parse(&data, start, nl).unwrap();
            start = nl + 1;
            n += 1;
        }
    }
    eprintln!("{n} values");
}

/// Parse + run a program on the port's VM + print, through the engine.
/// `cargo test --release --test io_throughput vm_sanity -- --ignored --nocapture`
#[test]
#[ignore]
fn vm_sanity() {
    use qj::jq::lang::execute::Jq;
    use qj::jq::lang::{CompileOptions, jq_compile_args};
    use qj::jq::value::print::dump_to_vec;

    struct W(Jq);
    impl parallel::RecordWorker for W {
        fn process(
            &mut self,
            v: qj::jq::value::Value,
            _meta: &parallel::RecordMeta<'_>,
            out: &mut Vec<u8>,
            _err: &mut Vec<u8>,
        ) -> i32 {
            self.0.start(v, 0);
            while let Some(Ok(v)) = self.0.next() {
                dump_to_vec(&v, &DumpOptions::compact(), out);
                out.push(b'\n');
            }
            0
        }
    }
    struct F(&'static str);
    impl parallel::WorkerFactory for F {
        type Worker = W;
        fn new_worker(&self) -> W {
            W(Jq::new(
                jq_compile_args(self.0.as_bytes(), &CompileOptions::new("/usr/bin")).unwrap(),
            ))
        }
    }
    let data = synthetic_ndjson(100_000);
    let mb = data.len() as f64 / 1e6;
    for program in [".", ".actor.login", "select(.public) | .id"] {
        for threads in [0, 16] {
            let t = Instant::now();
            let mut m = MemoryOpener::new();
            m.add("f", data.clone());
            let mut r =
                InputReader::with_opener(vec!["f".into()], ReaderOptions::default(), Box::new(m));
            let mut sink = Count(0);
            let opts = EngineOptions {
                threads,
                ..EngineOptions::default()
            };
            parallel::run(&mut r, &F(program), &mut sink, &opts);
            let dt = t.elapsed().as_secs_f64();
            eprintln!(
                "{program:24} {threads:2} threads: {:.0} MB/s ({} bytes out)",
                mb / dt,
                sink.0
            );
        }
    }
}

/// Where the time goes for one big document read from a file (the default
/// opener: memory-mapped), for the fast path and jq's parser port:
/// `QJ_SANITY_FILE=big.json cargo test --release --test io_throughput big_file_sanity -- --ignored --nocapture`
#[test]
#[ignore]
fn big_file_sanity() {
    let Some(path) = std::env::var_os("QJ_SANITY_FILE") else {
        eprintln!("set QJ_SANITY_FILE to a JSON file");
        return;
    };
    for fast in [true, false, true] {
        let t = Instant::now();
        let mut r = InputReader::new(vec![path.clone()], ReaderOptions::default());
        r.set_fast_path(fast);
        let mut values = Vec::new();
        while let Some(v) = r.next() {
            values.push(v.unwrap());
        }
        let read = t.elapsed().as_secs_f64();
        let t = Instant::now();
        drop(values);
        let dropped = t.elapsed().as_secs_f64();
        eprintln!(
            "fast path {fast:5}: read {read:.3} s, drop {dropped:.3} s ({:?})",
            r.stats()
        );
    }
}

struct Count(usize);

impl RecordSink for Count {
    fn record(&mut self, out: &[u8], _err: &[u8], _status: i32) -> ControlFlow<()> {
        self.0 += out.len();
        ControlFlow::Continue(())
    }
    fn parse_error(&mut self, _e: qj::jq::value::Error) -> ControlFlow<()> {
        ControlFlow::Break(())
    }
}
