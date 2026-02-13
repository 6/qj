# Benchmarking Strategy for jx

jx claims "10-50x faster on large inputs." This document defines how we
measure, track, and present that — with enough rigor to survive scrutiny
but without over-engineering for a young CLI tool.

---

## 1. Philosophy

### Be honest about where jx loses

ripgrep's benchmarks (BurntSushi) are the gold standard: they include
scenarios where ripgrep is slower than grep, explain WHY architecturally,
and publish raw data. jx should do the same. When jaq or gojq wins on a
filter, show it and explain the tradeoff (e.g., "jaq's evaluator is
faster on complex filters; jx wins on parse-dominated workloads").

### Be explicit about what's being measured

fd got burned by "misleading benchmarks" GitHub issues because caching
and thread count weren't documented. Every benchmark must state:
- Warm cache (hyperfine `--warmup 3`)
- Output destination (`> /dev/null` for throughput, pipe for realism)
- Thread count (single-threaded unless testing parallel mode)
- CPU architecture (ARM64 vs x86_64 matters for SIMD)

### Account for thermal throttling

hyperfine runs commands sequentially (all runs of cmd1, then all runs of
cmd2), not interleaved. On laptops, later commands run on a warmer CPU.
Running 10+ hyperfine invocations back-to-back compounds the effect.

Mitigations:
- **Cooldown between groups:** `sleep $COOLDOWN` (default 10s) between
  hyperfine invocations. Configurable via environment variable.
- **CI runners are less affected** — server-grade cooling, but shared
  runners have their own variance (~10-30%).

### Keep it simple

jaq's approach works at this stage: hyperfine tables in the README,
reproducible with a single script. We don't need a SaaS benchmarking
platform. The progression is:
1. **Done:** Shell scripts + hyperfine + auto-generated `BENCHMARKS.md`
2. **Next:** CI automation via `benchmark.yml` that commits `BENCHMARKS.md`
3. **Later:** Comprehensive cross-platform matrix (only if jx gets traction)

---

## 2. Benchmark Matrix

Three dimensions: **file size** x **filter complexity** x **output mode**.

### Files

Already exist or are trivial to generate via existing scripts:

| File | Size | Type | Generator |
|------|------|------|-----------|
| `twitter.json` | 631 KB | Single JSON | `bench/download_testdata.sh` |
| `citm_catalog.json` | 1.7 MB | Single JSON | `bench/download_testdata.sh` |
| `canada.json` | 2.2 MB | Single JSON (numeric-heavy) | `bench/download_testdata.sh` |
| `large_twitter.json` | ~49 MB | Single JSON | `bench/gen_large.sh` |
| `large.jsonl` | ~50 MB | NDJSON | `bench/gen_large.sh` |
| `100k.ndjson` | ~8 MB | NDJSON (synthetic) | `bench/generate_ndjson.sh` |
| `1m.ndjson` | ~82 MB | NDJSON (synthetic) | `bench/generate_ndjson.sh` |

**Future (for parallel benchmarks):**
- `xl.ndjson` — 500MB+ generated NDJSON, needed to show parallel scaling

### Filters (5 tiers)

These cover the full performance spectrum, from parse-dominated to
eval-dominated workloads. All use `twitter.json` / `large_twitter.json`
structure:

| Tier | Filter | What it tests |
|------|--------|---------------|
| 1. Identity compact | `-c '.'` | Passthrough fast path (simdjson::minify) |
| 2. Field extraction | `-c '.statuses'` | Passthrough candidate (path lookup + raw copy) |
| 3. Pipe + builtin | `.statuses\|length` | Minimal eval, parse still dominates |
| 4. Iterate + field | `.statuses[]\|.user.name` | Eval-heavy, many output values |
| 5. Select + construct | `.statuses[]\|select(.retweet_count>0)\|{user:.user.screen_name,n:.retweet_count}` | Complex eval, object construction |

**Why these tiers matter:** jx's advantage is largest at tiers 1-3 (SIMD
parsing dominates). At tiers 4-5, jaq's evaluator may be faster. Showing
both tells an honest story.

### Tools compared

All four are already used in `bench/run_bench.sh`:
- **jx** (this project)
- **jq** 1.7+ (reference implementation)
- **jaq** 2.x (fastest Rust alternative)
- **gojq** 0.12+ (Go alternative)

---

## 3. Benchmark Scripts

### Current state

| Script | Purpose |
|--------|---------|
| `bench/run_bench.sh` | Hyperfine: jx vs jq vs jaq vs gojq on twitter.json + large_twitter.json |
| `bench/compare_tools.sh` | Older baseline script (jq vs jaq only, no jx) |
| `bench/download_testdata.sh` | Downloads twitter.json, citm_catalog.json, canada.json |
| `bench/gen_large.sh` | Generates large_twitter.json (~49MB) + large.jsonl (~50MB) |
| `bench/generate_ndjson.sh` | Generates 100k.ndjson + 1m.ndjson via gen_ndjson binary |
| `bench/build_cpp_bench.sh` | Builds C++ simdjson baseline benchmark |
| `benches/parse_throughput.rs` | Rust benchmark: simdjson vs serde_json parse speed |

### `bench/bench.sh` (implemented)

Replaces `bench/run_bench.sh` as the primary benchmark script:

1. **Correctness validation:** Compares jx vs jq output for every
   filter+file combo before timing
2. **JSON export:** `hyperfine --export-json` for every run, saved to
   `bench/results/`
3. **Platform tagging:** Captures `uname -ms` and date in output
4. **Full tier coverage:** All 5 filter tiers on both small + large files
5. **Writes `BENCHMARKS.md`:** Markdown table with bold jx column,
   auto-generated from hyperfine results
6. **Cooldown:** `sleep $COOLDOWN` (default 10s) between hyperfine
   invocations to mitigate thermal buildup (see "Account for thermal
   throttling" above)

Existing helper scripts (`download_testdata.sh`, `gen_large.sh`,
`generate_ndjson.sh`, `build_cpp_bench.sh`) stay unchanged.

### Results directory

```
bench/results/
  2025-06-15-darwin-arm64.json
  2025-06-15-linux-x86_64.json
  ...
```

Gitignored. CI uploads these as artifacts.

---

## 4. GitHub Actions CI

### a. `checks.yml` — every push/PR (fast, no benchmarks)

Runs on every push and PR. Tests correctness only.

```yaml
name: Checks
on:
  push:
    branches: [main]
  pull_request:

jobs:
  build-and-test:
    strategy:
      matrix:
        os: [ubuntu-24.04, macos-26]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: true

      - uses: jdx/mise-action@v2

      - uses: Swatinem/rust-cache@v2

      - name: Build
        run: cargo build --release

      - name: Test
        run: cargo test

      - name: Clippy
        run: cargo clippy --release -- -D warnings

      - name: Check benchmarks compile
        run: cargo bench --no-run
```

Notes:
- `jdx/mise-action@v2` installs Rust from `mise.toml` (pinned version),
  keeping CI in sync with local dev.
- `macos-26` is macOS Tahoe (ARM64), `ubuntu-24.04` is the current LTS.
- `cargo bench --no-run` validates benchmark code compiles without
  actually running (what ripgrep and tokio do).
- Rust cache via `Swatinem/rust-cache` for fast rebuilds.

### b. `benchmark.yml` — on push to main + manual trigger

Runs the full benchmark suite. No cron schedule — wasteful for a project
with infrequent commits. Path filter ensures it only runs when code
changes.

```yaml
name: Benchmarks
on:
  push:
    branches: [main]
    paths: ['src/**', 'bench/**', 'benches/**', 'Cargo.toml', 'Cargo.lock']
  workflow_dispatch:

permissions:
  contents: write

jobs:
  benchmark:
    strategy:
      matrix:
        include:
          - os: ubuntu-24.04
            platform: linux-x86_64
          - os: macos-26
            platform: darwin-arm64
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: true

      - uses: jdx/mise-action@v2

      - uses: Swatinem/rust-cache@v2

      - name: Build release
        run: cargo build --release

      - name: Install benchmark tools
        run: |
          if [ "$RUNNER_OS" = "Linux" ]; then
            sudo apt-get update && sudo apt-get install -y jq
            # jaq: download prebuilt binary
            curl -sL https://github.com/01mf02/jaq/releases/latest/download/jaq-x86_64-unknown-linux-gnu \
              -o /usr/local/bin/jaq && chmod +x /usr/local/bin/jaq
            # gojq: download prebuilt binary
            GOJQ_VERSION=$(curl -s https://api.github.com/repos/itchyny/gojq/releases/latest | jq -r .tag_name)
            curl -sL "https://github.com/itchyny/gojq/releases/download/${GOJQ_VERSION}/gojq_${GOJQ_VERSION#v}_linux_amd64.tar.gz" \
              | tar xz -C /tmp && sudo mv /tmp/gojq_*/gojq /usr/local/bin/
            # hyperfine
            cargo install hyperfine
          else
            brew install jq hyperfine
            brew install jaq || true
            brew install gojq || true
          fi

      - name: Download/generate test data
        run: |
          bash bench/download_testdata.sh
          bash bench/gen_large.sh
          bash bench/generate_ndjson.sh

      - name: Cache test data
        uses: actions/cache@v4
        with:
          path: bench/data
          key: bench-data-v1

      - name: Run benchmarks
        run: bash bench/bench.sh

      - name: Commit BENCHMARKS.md
        run: |
          git config user.name "github-actions[bot]"
          git config user.email "github-actions[bot]@users.noreply.github.com"
          git add BENCHMARKS.md
          git diff --staged --quiet || git commit -m "Update BENCHMARKS.md [skip ci]"
          git push
```

Notes:
- No `schedule:` trigger. Path filter on `src/**`, `bench/**`, `Cargo.toml`
  ensures benchmarks only run when code actually changes.
- `bench/bench.sh` writes `BENCHMARKS.md` directly — no external actions needed.
- `[skip ci]` in the commit message prevents an infinite loop.
- `git diff --staged --quiet ||` ensures no empty commits when results are unchanged.

---

## 5. Regression Tracking

No external tooling. Regression tracking is just git history:

```
git log -p BENCHMARKS.md
```

This shows exactly what changed and when. Each CI run that changes
performance numbers produces a commit with the diff visible inline.

For a project at this stage, this is sufficient. If jx gets enough
contributors that regressions become frequent, upgrade to
`benchmark-action/github-action-benchmark` with gh-pages charts later.

---

## 6. Presenting Results

### `BENCHMARKS.md` (auto-generated by CI)

`bench/bench.sh` writes a markdown file with tables per platform:

```markdown
# Benchmarks

> Auto-generated by CI. Do not edit manually.
> Last updated: 2025-06-15 on darwin-arm64

## darwin-arm64

| Filter | File | jx | jq | jaq | gojq |
|--------|------|----|----|-----|------|
| -c '.' | twitter.json | 1.2ms | 45ms | 8ms | 12ms |
| -c '.' | large_twitter.json | 18ms | 1.1s | 180ms | 320ms |
| .statuses[]\|.user.name | twitter.json | 3ms | 48ms | 5ms | 14ms |
| .statuses[]\|.user.name | large_twitter.json | 85ms | 1.8s | 210ms | 580ms |
```

*(Numbers are illustrative — update with actual measurements.)*

Since both matrix jobs write to the same file, the workflow needs to
handle this. Simplest approach: each job writes a platform-specific
section, and only the last job to finish commits. Alternatively, use a
separate `update-benchmarks` job that depends on both matrix jobs,
downloads their artifacts, merges them into one `BENCHMARKS.md`, and
commits.

### Commentary

Include a short "Understanding the numbers" section at the top of
`BENCHMARKS.md`:
- "jx is fastest on parse-dominated workloads (tiers 1-3) thanks to SIMD"
- "On complex filters (tier 5), jaq's evaluator can be faster"
- "NDJSON parallel processing (when implemented) will show the biggest gains"
- "All benchmarks: warm cache, output to /dev/null, single-threaded"

---

## 7. What NOT to Do

| Temptation | Why not |
|------------|---------|
| CodSpeed / bencher.dev | Overkill for a CLI tool at this stage. Adds a SaaS dependency. |
| gh-pages / benchmark-action | Extra infrastructure (gh-pages branch, deploy permissions). `BENCHMARKS.md` in the repo is simpler and more discoverable. Upgrade later if needed. |
| Self-hosted runners | Maintenance burden. Shared runners are fine — CI numbers are directional, not absolute. |
| Criterion microbenchmarks for filter eval | Hyperfine end-to-end is what users experience. Microbenchmarks are useful for optimizing internals but shouldn't be the public-facing story. |
| Windows CI | Not a target platform (PLAN.md). Would need MSVC simdjson build, not worth it yet. |
| Scheduled CI runs | Wasteful for a project with infrequent commits. `push` to main with path filters is sufficient. |
| Blocking PRs on perf regression | Shared runner variance is 10-30%. A hard gate would create false-positive friction. |

---

## 8. Implementation Order

1. ~~**Create `bench/bench.sh`**~~ — **Done.** Correctness checks, 5 filter tiers, writes `BENCHMARKS.md`, bold jx column.
2. ~~**Add `bench/results/` to `.gitignore`**~~ — **Done.**
3. ~~**Create `.github/workflows/checks.yml`**~~ — **Done.** Build + test on ubuntu-24.04 + macos-26, mise-action, rust-cache.
4. **Create `.github/workflows/benchmark.yml`** — full benchmark suite, commits `BENCHMARKS.md`
5. **Update README** to link to `BENCHMARKS.md`
