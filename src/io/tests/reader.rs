use super::{Delivery, MemFile, mem_reader, show, simulate_cli};
use crate::io::reader::ReaderOptions;

type J = serde_json::Value;

fn hex_decode(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
        .collect()
}

fn field_bytes(c: &J, key: &str) -> Vec<u8> {
    if let Some(s) = c.get(key).and_then(J::as_str) {
        return s.as_bytes().to_vec();
    }
    if let Some(h) = c.get(format!("{key}_hex")).and_then(J::as_str) {
        return hex_decode(h);
    }
    panic!("case has no {key}: {c}")
}

const DELIVERIES: [Delivery; 5] = [
    Delivery::Whole,
    Delivery::Stream { seed: 1, max: 1 },
    Delivery::Stream { seed: 2, max: 7 },
    Delivery::Stream { seed: 3, max: 100 },
    Delivery::Stream { seed: 4, max: 5000 },
];

/// jq 1.8.1's CLI on stdin (`src/jq/value/testdata/parse_cli.json`, recorded
/// by track V): every case, every delivery, with and without the fast path.
#[test]
fn parse_cli_fixture() {
    let fixture: J =
        serde_json::from_str(include_str!("../../jq/value/testdata/parse_cli.json")).unwrap();
    let mut failures = Vec::new();
    let mut n = 0;
    for c in fixture["cases"].as_array().unwrap() {
        let input = field_bytes(c, "in");
        let want_out = field_bytes(c, "out");
        let want_err = field_bytes(c, "err");
        let flags: Vec<&str> = c["flags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap())
            .collect();
        let has = |f: &str| flags.contains(&f);
        let opts = ReaderOptions {
            seq: has("--seq"),
            stream: has("--stream"),
            stream_errors: has("--stream-errors"),
            ..Default::default()
        };
        for delivery in DELIVERIES {
            for fast in [true, false] {
                n += 1;
                let (mut r, msgs) = mem_reader(
                    &["-"],
                    vec![("-".into(), MemFile::Data(input.clone()))],
                    opts,
                    delivery,
                    fast,
                );
                let (out, err) = simulate_cli(&mut r, &msgs, opts.seq);
                if out != want_out || err != want_err {
                    failures.push(format!(
                        "{flags:?} {:?} {delivery:?} fast={fast}:\n  got out {:?} err {:?}\n  jq  out {:?} err {:?}",
                        show(&input[..input.len().min(80)]),
                        show(&out[..out.len().min(300)]),
                        show(&err),
                        show(&want_out[..want_out.len().min(300)]),
                        show(&want_err)
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {n} runs differ from jq:\n{}",
        failures.len(),
        failures[..failures.len().min(20)].join("\n")
    );
}
