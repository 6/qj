# Benchmarks

## Results

> Run on dedicated local hardware. Not yet populated — run `bash benches/bench.sh` to generate.

## Methodology

`bench.sh` uses [hyperfine](https://github.com/sharkdeveloper/hyperfine) to measure end-to-end wall-clock time (process spawn + read + parse + filter + format + write to /dev/null) across jx, jq, jaq, and gojq on a mix of small and large JSON/JSONL files.

## Reproducing

Prerequisites:

```bash
brew install hyperfine jq jaq gojq coreutils
```

Generate test data and run:

```bash
bash benches/download_testdata.sh   # twitter.json, citm_catalog.json, canada.json
bash benches/gen_large.sh           # ~49MB large_twitter.json, large.jsonl
bash benches/bench.sh               # runs hyperfine, writes benches/results.md
```

## Other benchmarks

### Parse throughput (simdjson vs serde_json)

Microbenchmark comparing raw parse speed without filter evaluation:

```bash
bash benches/generate_ndjson.sh     # 100k.ndjson, 1m.ndjson
cargo bench --bench parse_throughput
```

### C++ baseline (no FFI overhead)

Measures simdjson directly from C++ to quantify FFI overhead:

```bash
bash benches/build_cpp_bench.sh
./benches/bench_cpp
```

### Profiling a single run

```bash
./target/release/jx --debug-timing -c '.' benches/data/large_twitter.json > /dev/null
```

### Ad-hoc comparison

```bash
hyperfine --warmup 3 \
  './target/release/jx ".field" test.json' \
  'jq ".field" test.json' \
  'jaq ".field" test.json'
```
