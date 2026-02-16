# Multi-Format Support Plan

## Motivation

qj's current value proposition is speed on JSON/NDJSON. Adding input format support
for YAML, TOML, CSV/TSV, and Parquet would make it a "one tool for all structured data"
option — jq syntax applied to any format, with speed as a bonus.

The architecture is clean: convert at the input boundary to `serde_json::Value`, then
the entire filter/eval/output pipeline works unchanged.

---

## Priority 1: YAML + TOML (trivial effort, table-stakes)

### Why

- Config files (k8s manifests, Cargo.toml, docker-compose.yml) are the most common
  non-JSON structured data people work with.
- Files are almost always <1MB, so speed is irrelevant — this is purely a convenience feature.
- gojq already has `--yaml-input`/`--yaml-output`. Not having it makes qj look incomplete.
- Both are ~10 lines of glue code each.

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
explicit flags as override. Auto-detect is what yq and dasel do.

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

## Priority 2: CSV/TSV (moderate effort, good fit)

### Why

- CSV files are routinely GB-scale (log exports, analytics dumps, database extracts).
- Row-based processing maps perfectly to qj's parallel NDJSON architecture.
- No existing tool combines fast parallel CSV processing with jq query syntax.
- The pitch: `qj --csv 'select(.status == "500") | {url, timestamp}' access_log.csv`

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

### SIMD CSV: considered and deferred

SIMD CSV parsers exist ([simdcsv](https://github.com/geofflangdale/simdcsv) by Geoff
Langdale, [zsv](https://github.com/liquidaty/zsv), [csimdv-rs](https://github.com/juliusgeo/csimdv-rs))
but integrating one via FFI is a substantial engineering effort:
- Quote-aware chunk splitting for parallel processing is harder than NDJSON newline splitting.
- The `csv` crate (by BurntSushi) is already very fast and production-hardened.
- The bottleneck shifts to jq evaluation anyway, same as NDJSON.
- Engineering weeks for marginal parse-speed gains over `csv` crate + rayon.

Decision: **use BurntSushi's `csv` crate + rayon parallelism**. "Fast enough" + jq syntax
is the pitch, not "fastest CSV parser." Revisit SIMD if CSV becomes a major use case.

### Implementation

```rust
let mut rdr = csv::ReaderBuilder::new()
    .delimiter(if tsv { b'\t' } else { b',' })
    .from_reader(input);

for result in rdr.deserialize() {
    let record: HashMap<String, serde_json::Value> = result?;
    // → run jq filter on record
}
```

Parallel processing: split file into chunks (respecting quoted newlines), parse each
chunk's rows independently with rayon, merge output in order. Same architecture as NDJSON.

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

## Priority 3: Parquet (high differentiation, heavy dependency)

### Why

- Parquet is the lingua franca of data engineering. Files are routinely GB+.
- **No jq-syntax tool exists for Parquet.** The only options are SQL tools (duckdb,
  datafusion-cli, spark) which are a different paradigm entirely.
- "Query Parquet files with jq syntax" is a unique and compelling pitch.
- Data engineers who know jq but don't want to spin up duckdb for a quick query.

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

1. **YAML + TOML** — ship together, minimal effort, immediate broadening of appeal.
2. **CSV/TSV** — moderate effort, good fit for qj's parallel architecture.
3. **Parquet** — investigate dependency weight, ship as optional feature if feasible.

## CLI Design

Prefer auto-detection from file extension with explicit override flags:

```
qj '.key' file.yaml          # auto-detect YAML
qj '.key' file.toml          # auto-detect TOML
qj '.col' file.csv           # auto-detect CSV
qj '.col' file.parquet       # auto-detect Parquet
qj --yaml-input '.key' -     # explicit for stdin
qj --csv --csv-separator ';' '.col' data.csv  # explicit with options
```

For output formats (later):
```
qj --yaml-output '.key' file.json    # JSON in, YAML out
qj --csv-output '.' file.json        # JSON array in, CSV out
```
