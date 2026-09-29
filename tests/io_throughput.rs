//! Sanity timings for the src/io input layer, with the binary's allocator.
//! `#[ignore]`d: these are not benchmarks (the numbers are noisy), just a
//! quick way to see where time goes:
//! `cargo test --release --test io_throughput -- --ignored --nocapture`

use mimalloc::MiMalloc;
use std::time::Instant;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use qj::io::simd::SimdParser;
use qj::jq::value::{ParseFlags, Parser};

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
    }
}
