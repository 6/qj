# Multi-Format Support Plan

## Motivation

qj's current value proposition is speed on JSON/NDJSON. Adding input format support
for CSV/TSV, Parquet, and optionally YAML/TOML would make it a "one tool for all
structured data" option — jq syntax applied to any format, with speed as a bonus.

The architecture is clean: convert at the input boundary to `serde_json::Value`, then
the entire filter/eval/output pipeline works unchanged.

---

## Priority 1: CSV/TSV (moderate effort, strongest fit)

### Why

- CSV files are routinely GB-scale (log exports, analytics dumps, database extracts).
- Row-based processing maps perfectly to qj's parallel NDJSON architecture.
- No existing tool combines fast parallel CSV processing with jq query syntax.
- xsv/qsv have fragmented CLIs (separate subcommand for each operation); jq syntax
  is more expressive and composable.

### Use cases

**Filter + reshape server access logs (GB-scale):**
```bash
# "Show me all 500 errors with their URLs and timestamps"
# xsv: multiple piped subcommands
xsv search -s status "500" access.csv | xsv select url,timestamp

# qj: one composable expression
qj --csv 'select(.status == "500") | {url, timestamp}' access.csv
```

**Computed fields from analytics exports:**
```bash
# "Calculate conversion rate per campaign from a marketing dump"
qj --csv '{campaign: .campaign_name, rate: ((.conversions | tonumber) / (.clicks | tonumber) * 100)}' campaigns.csv
```

**Aggregate + filter database dumps:**
```bash
# "Unique error codes from a 2GB log export"
qj --csv '.error_code' errors.csv | sort -u

# "Filter rows with complex conditions"
qj --csv 'select((.amount | tonumber) > 1000 and .currency == "USD") | {id, amount}' transactions.csv
```

**Convert CSV to NDJSON for downstream pipelines:**
```bash
# Every row becomes a JSON object — natural format bridge
qj --csv '.' data.csv > data.ndjson
```

**TSV from command output (ps, lsof, database CLIs):**
```bash
# psql outputs TSV with \t separator
psql -t -A -F $'\t' -c "SELECT id, name, email FROM users" | qj --tsv 'select(.email | endswith("@company.com"))'
```

### Competitive landscape

| Tool | Parse speed | Query language | Transformation |
|------|------------|----------------|----------------|
| xsv | Fast (~1-2 GB/s) | Very limited CLI flags | Minimal |
| qsv | Fast, some SIMD | Per-command filters | Moderate |
| zsv | SIMD (~4 GB/s) | SQL (via SQLite load) or regex | Full SQL but with load overhead |
| zsv jq | SIMD parse + jq | jq (shells out to jq) | Full jq, but single-threaded jq |
| **qj** | csv crate + rayon | jq syntax | Full jq, parallel |

Key insight: zsv already acknowledges jq is useful for CSV (`zsv jq` subcommand) but
shells out to single-threaded jq. qj with parallel row processing would be strictly
faster for jq-style queries on CSV, without needing SIMD CSV parsing.

### SIMD CSV parsing

SIMD CSV parsers exist and the FFI integration pattern is already established in qj
(simdjson bridge.cpp → bridge.rs → build.rs). Adding another C library follows the
same template.

**Best candidates:**

- **[zsv](https://github.com/liquidaty/zsv)** — C library, MIT licensed, actively
  maintained, claims "world's fastest CSV parser." ~4 GB/s throughput. Has a clean
  row-iteration API. Best choice for FFI integration.
- **[csimdv-rs](https://github.com/juliusgeo/csimdv-rs)** — Pure Rust, uses PCLMULQDQ
  trick from simdjson. Up to 60% faster than other SIMD parsers on x86_64 with AVX-512.
  Works on aarch64. No FFI needed but less mature.
- **[simdcsv](https://github.com/geofflangdale/simdcsv)** — by Geoff Langdale
  (co-creator of simdjson). Never finished, but the techniques are proven.

**Parallel chunk splitting:** For NDJSON, qj splits on `\n` — trivial. For CSV, the
only added complexity is quoted fields can contain newlines. The standard approach is
split on `\n` then verify you're not inside a quoted field (scan for unbalanced quotes,
adjust boundary). In practice quoted newlines are rare; this works for 99.9% of real
files. Same speculative approach zsv and others use.

**Estimated effort:** ~3 days following the established simdjson FFI pattern:
1. Integrate zsv via FFI, get single-threaded row iteration working
2. Wire into existing parallel architecture with quote-aware chunk splitting
3. CLI flags, auto-detect, type inference, tests

### Implementation

FFI approach (zsv):
```c
// bridge_csv.cpp — C-linkage wrapper around zsv
extern "C" int csv_open(const char* path, char delimiter);
extern "C" int csv_next_row(/* row handle */);
extern "C" const char* csv_get_field(int col, size_t* len);
extern "C" void csv_close();
```

```rust
// Rust side — convert each row to serde_json::Value (or flat key-value pairs)
let headers = csv_get_headers();
for row in csv_rows() {
    let obj: Map<String, Value> = headers.iter().zip(row.fields())
        .map(|(k, v)| (k.clone(), Value::String(v.to_string())))
        .collect();
    // → run jq filter on obj
}
```

Alternative: use csimdv-rs directly in Rust (no FFI, but less battle-tested).

Parallel processing: mmap the file, split into ~1MB chunks at newline boundaries
(with quote-awareness), parse each chunk's rows independently with rayon, merge
output in order. Same architecture as NDJSON.

### CLI flags

```
qj --csv 'select(.age > 30) | {name, email}' users.csv
qj --tsv '.score | tonumber | select(. > 90)' results.tsv
qj --csv --csv-separator ';' '.' european_data.csv
```

Or auto-detect from `.csv`/`.tsv` extension.

### Type inference

CSV has no type system. Options:

1. **Everything as strings** (safe, round-trippable, predictable).
2. **Auto-parse** numbers/booleans (convenient, but ZIP codes like `01234` → `1234`).
3. **Flag-controlled**: `--csv-auto-parse` (default off, strings-only).

Recommendation: strings by default, `--csv-auto-parse` flag for convenience. Users can
always do `.age | tonumber` in the jq filter for explicit conversion.

### Gotchas

- **Header row**: first row = column names by default. Flag `--no-header` for headerless
  CSVs (columns become `_0`, `_1`, etc. or array-per-row).
- **Empty fields**: become empty string `""`, not null. Debatable — could offer a flag.
- **Quoted newlines**: the `csv` crate handles these correctly, but chunk splitting for
  parallel processing must account for them.
- **Output shape**: each row → one JSON object. Whole file with `--slurp` → array of objects.

### Dependency cost

- `csv`: very light (<1MB total tree). Already depends on `serde`, `memchr`.

---

## Priority 2: Parquet (high differentiation, heavy dependency)

### Why

- Parquet is the lingua franca of data engineering. Files are routinely GB+.
- **No jq-syntax tool exists for Parquet.** The only options are SQL tools (duckdb,
  datafusion-cli, spark) which are a different paradigm entirely.
- "Query Parquet files with jq syntax" is a unique and compelling pitch.
- Data engineers who know jq but don't want to spin up duckdb for a quick query.

### Use cases

**Quick inspection of data warehouse exports:**
```bash
# "What does this Parquet file look like?" — no SQL, no Python, no notebooks
qj '.' events.parquet | head -5

# "What columns/keys are in this data?"
qj 'keys' events.parquet | head -1
```

**Ad-hoc filtering without spinning up a query engine:**
```bash
# "Find all failed transactions from last month's export"
# duckdb: install duckdb, write SQL, manage quoting
duckdb -c "SELECT * FROM 'transactions.parquet' WHERE status = 'failed' AND amount > 1000"

# qj: jq syntax you already know
qj 'select(.status == "failed" and .amount > 1000)' transactions.parquet
```

**Pipeline bridge — Parquet to NDJSON:**
```bash
# Convert Parquet to NDJSON for tools that don't read Parquet
qj -c '.' events.parquet | kafka-producer --topic events

# Extract + reshape before sending downstream
qj -c '{user: .user_id, event: .event_type, ts: .timestamp}' events.parquet | downstream-tool
```

**Data sampling and exploration in shell scripts:**
```bash
# "Sample 100 rows matching a condition" — useful in CI, data validation scripts
qj 'select(.country == "US") | {id, name}' users.parquet | head -100

# Combine with standard unix tools
qj '.revenue' sales.parquet | sort -n | tail -10  # top 10 revenues
```

### Competitive landscape

| Tool | Query language | Setup |
|------|----------------|-------|
| duckdb | SQL | Install duckdb, write SQL |
| datafusion-cli | SQL | Install datafusion, write SQL |
| polars CLI | Python/SQL | Python environment |
| **qj** | jq syntax | Already installed, just works |

### Implementation sketch

Use the `parquet` crate (part of arrow-rs):
```rust
use parquet::file::reader::SerializedFileReader;
// Read row groups → convert each row to serde_json::Value → jq filter
```

Row groups in Parquet map naturally to parallel processing chunks.

### Open questions

- **Dependency weight**: arrow-rs ecosystem is ~500K+ SLoC. This is massive. Could make
  it an optional cargo feature (`--features parquet`) to keep the default binary lean.
- **Columnar → row conversion overhead**: Parquet is columnar; converting to row-based
  JSON objects has inherent overhead. For simple column selections, a smarter approach
  would read only the needed columns (predicate pushdown).
- **Nested types**: Parquet supports nested structs, lists, maps — these map well to JSON.
- **Large integers / decimals**: Parquet has decimal128, int96, etc. Need to decide on
  JSON representation (string vs number vs truncate).

### CLI

```
qj '.name' users.parquet
qj 'select(.age > 30) | {name, email}' users.parquet
```

Auto-detect from `.parquet` extension.

### Recommendation

Investigate as a cargo feature flag. If the dep weight is manageable with feature gating,
this is the highest-differentiation format to support. If arrow-rs pulls in too much,
consider using a lighter Parquet reader like `parquet2` (deprecated but smaller).

---

## Priority 3: YAML + TOML (trivial effort, checkbox feature)

### Why

- gojq already has `--yaml-input`/`--yaml-output`. Not having it makes qj look incomplete.
- Both are ~10 lines of glue code each.
- Trivial to implement, low maintenance burden.

### Honest assessment

Most YAML/TOML use cases are better served by grep, opening the file, or yq.
Files are almost always <1MB, so speed is irrelevant. Nobody will switch to qj because
it reads YAML. This is a checkbox feature, not a use case driver.

### Use cases (where jq syntax is actually better than grep)

**Multi-document k8s manifests — filter across many resources in one file:**
```bash
# "Find all Deployments with >2 replicas across a multi-doc manifest"
# grep can't do this — it doesn't understand document boundaries or nesting
qj --yaml-input 'select(.kind == "Deployment" and .spec.replicas > 2) | .metadata.name' k8s-resources.yaml
```

**Extract deeply nested values in shell scripts (more robust than grep):**
```bash
# grep breaks if indentation changes or value is on next line
# jq syntax gives you structural access
qj --toml-input '.workspace.members[]' Cargo.toml
qj --yaml-input '.services | keys[]' docker-compose.yml
```

**Format conversion in pipelines:**
```bash
# YAML → JSON for tools that only accept JSON
qj --yaml-input '.' config.yaml > config.json

# Extract and reshape config for another tool
qj --toml-input '{name: .package.name, deps: (.dependencies | keys)}' Cargo.toml
```

**Validating config values in CI:**
```bash
# "Fail CI if any service exposes port 80"
qj --yaml-input '.services[] | select(.ports[]? | startswith("80:"))' docker-compose.yml && exit 1
```

### Implementation

**YAML** — use `serde-saphyr` (replacement for deprecated `serde_yaml`):
```rust
let value: serde_json::Value = serde_saphyr::from_str(yaml_str)?;
```
Multi-document YAML (`---` separated) maps naturally to NDJSON-style per-document processing.

**TOML** — use `toml` crate (v0.8+):
```rust
let toml_val: toml::Value = toml::from_str(toml_str)?;
let json_val: serde_json::Value = serde_json::to_value(&toml_val)?;
```
Two-step because TOML's `Datetime` type has no JSON equivalent (becomes a string).

### CLI flags

```
qj --yaml-input '.services.app.ports' docker-compose.yml
qj --toml-input '.package.version' Cargo.toml
```

Or auto-detect from file extension (`.yml`/`.yaml` → YAML, `.toml` → TOML) with
explicit flags as override.

### Gotchas

- **YAML "Norway problem"**: bare `NO`, `YES`, `on`, `off` are booleans in YAML 1.1.
  Use `strict_booleans: true` in serde-saphyr to only recognize `true`/`false` (YAML 1.2).
- **TOML datetimes**: become JSON strings, losing type information.
- **TOML has no null**: missing keys are absent, not null. This is fine for querying.
- **TOML NaN/Infinity**: floats support `nan`, `+inf`, `-inf` but JSON doesn't.
  Need special handling (error or convert to null).

### Dependency cost

- `serde-saphyr`: ~302K SLoC across dep tree (heavier than expected, but acceptable).
- `toml`: ~51K SLoC (moderate, pulls in `winnow` parser combinator).

### Output support (optional, later)

`--yaml-output` and `--toml-output` are nice-to-haves but lower priority than input.
Output is just `serde_yaml::to_string(&value)` / `toml::to_string(&value)`.

---

## Won't Do

### XML

**Reason: fundamentally lossy mapping, high complexity, low ROI.**

XML → JSON conversion is inherently ambiguous:

- **Single child vs array**: `<items><item>A</item></items>` → `{"item": "A"}` but
  `<items><item>A</item><item>B</item></items>` → `{"item": ["A", "B"]}`. The schema
  changes based on cardinality. Every tool handles this differently.
- **Attributes vs elements**: XML attributes have no JSON equivalent. Conventions vary
  (`+@attr`, `@attr`, `$`, `-attr`). No standard.
- **Mixed content**: `<p>Hello <b>world</b>!</p>` has interleaved text and elements.
  No clean JSON representation.
- **Namespaces**: flattened into prefixed keys or stripped entirely.
- **Text content naming**: synthetic keys like `+content`, `$text`, `#text`.

Every tool (yq, dasel, xmltodict) makes different opinionated choices and users are
never fully satisfied. The Rust crate situation reflects this: `quick-xml`'s serde
support can't correctly deserialize to `serde_json::Value` (known issue #231), and
`quickxml_to_serde` uses its own conversion logic with the heaviest dep weight (~104K SLoC).

Tools like yq already handle XML well. Users working with XML regularly already have
tooling. The juice isn't worth the squeeze.

### MessagePack / CBOR / Other Binary Formats

Niche use cases. Can be piped through conversion tools. Not worth the dependency or
maintenance burden.

### HCL / INI / Properties

Too niche. HCL is Terraform-specific. INI/properties are flat key-value, barely
"structured data."

---

## Implementation Order

1. **CSV/TSV** — strongest fit for qj's speed + parallelism story. Real large-file use cases.
2. **Parquet** — highest differentiation. No jq-syntax competitor exists. Investigate dep weight.
3. **YAML + TOML** — trivial to add, ship when convenient. Checkbox feature.

## CLI Design

Prefer auto-detection from file extension with explicit override flags:

```
qj '.col' file.csv           # auto-detect CSV
qj '.col' file.tsv           # auto-detect TSV
qj '.col' file.parquet       # auto-detect Parquet
qj '.key' file.yaml          # auto-detect YAML
qj '.key' file.toml          # auto-detect TOML
qj --csv '.col' -            # explicit for stdin
qj --csv --csv-separator ';' '.col' data.csv  # explicit with options
```

For output formats (later):
```
qj --yaml-output '.key' file.json    # JSON in, YAML out
qj --csv-output '.' file.json        # JSON array in, CSV out
```
