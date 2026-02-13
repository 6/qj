use std::env;
use std::io::{self, BufWriter, Write};

fn main() {
    let args: Vec<String> = env::args().collect();
    let count: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(100_000);

    let stdout = io::stdout();
    let mut out = BufWriter::with_capacity(1 << 20, stdout.lock());

    // Deterministic pseudo-random via simple LCG — no external deps needed.
    let mut rng: u64 = 42;
    let names = [
        "alice", "bob", "charlie", "diana", "eve", "frank", "grace", "heidi",
    ];
    let cities = [
        "New York", "London", "Tokyo", "Paris", "Berlin", "Sydney", "Toronto", "Mumbai",
    ];

    for i in 0..count {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let name = names[(rng >> 32) as usize % names.len()];
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let city = cities[(rng >> 32) as usize % cities.len()];
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let age = 18 + (rng >> 32) % 60;
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let score = (rng >> 32) as f64 / u32::MAX as f64 * 100.0;

        writeln!(
            out,
            r#"{{"id":{i},"name":"{name}","city":"{city}","age":{age},"score":{score:.2},"active":{active}}}"#,
            active = if i % 3 == 0 { "true" } else { "false" }
        )
        .unwrap();
    }
}
