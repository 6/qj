# Feedback & Positioning Notes

## Does this project make sense given Parquet/DuckDB/etc.?

### Where Parquet/DuckDB/etc. are eating this lunch

- If someone has a 6GB NDJSON file and wants to run analytical queries repeatedly, converting to Parquet once and using DuckDB/Polars/ClickHouse Local is objectively better — columnar storage, predicate pushdown, orders of magnitude faster for aggregations.
- DuckDB can even read NDJSON directly (`SELECT * FROM 'file.ndjson' WHERE ...`), so the "but I don't want to convert" friction is shrinking.
- The data engineering world is trending hard toward columnar formats. Large-scale NDJSON as a query target is increasingly a transitional state, not a destination.

### Where qj still makes sense

- **Ad-hoc shell pipelines.** `curl api | qj '.results[].name'` — you're not going to fire up DuckDB for that. jq syntax is concise and people know it.
- **Streaming / ephemeral data.** Tailing logs, processing Kafka output, CI pipelines — data that flows through and doesn't sit on disk. DuckDB can't tail a stream.
- **One-shot exploration.** You get handed a JSON file and want to poke at it. The cognitive overhead of jq is lower than SQL for simple field extraction and transformation.
- **Transformation, not analytics.** Reshaping data (`{user: .actor.login, event: .type}`), filtering streams, format conversion — jq syntax is more natural than SQL for these. DuckDB is a query engine; qj is a transform tool.
- **Environments where jq is already embedded.** Thousands of shell scripts, Makefiles, and CI configs use jq. A drop-in replacement that's faster is a real value prop with zero migration cost.

### Realistic framing

The "large NDJSON benchmark" angle (6GB GH Archive, etc.) is impressive for marketing but is probably not the core use case. The real value is being a better jq for the millions of small-to-medium JSON tasks people already use jq for, where the alternative isn't Parquet — it's just jq being slow or annoying.

The NDJSON speed story is still real for streaming/one-shot scenarios (log tailing, CI pipelines, one-time ETL transforms) — just not for repeated analytical queries where columnar tools win.

### The multi-format answer

Adding CSV/TSV and Parquet support shifts the positioning from "faster jq" to something DuckDB doesn't occupy:

**"Universal CLI for structured data queries using jq syntax."**

- DuckDB's query language is SQL. qj's is jq. Different paradigms, different audiences.
- People who know jq syntax (millions) get to use it on CSV, Parquet, YAML — not just JSON.
- Streaming-first architecture works on pipes, tails, ephemeral data where DuckDB can't.
- The competition for CSV queries is xsv/qsv (fragmented CLIs) and `zsv sql` (SQLite load overhead). qj with parallel processing and jq syntax fills a gap.
- For Parquet, no jq-syntax tool exists at all. `qj 'select(.status == "failed")' data.parquet` has zero competition.

The three-layer positioning:
1. **Drop-in jq replacement** — same syntax, 2-150x faster (the base)
2. **Universal structured data tool** — CSV, Parquet, YAML, TOML all queryable with jq syntax (the expansion)
3. **Streaming-first** — works on pipes, tails, ephemeral data where analytical engines can't (the moat)

### CSV speed: can we compete with zsv?

zsv claims ~4 GB/s single-threaded SIMD CSV parsing. For raw parse throughput, we can't
match that — we have the overhead of converting rows to a queryable representation.

But for end-to-end query performance, we can match or exceed zsv:
- **Parallelism.** zsv is mostly single-threaded. qj with rayon on 10 cores can process
  chunks concurrently. Even at 1/3 per-core throughput, aggregate is 3x faster.
- **zsv sql is slow.** Its powerful query path loads into SQLite first — seconds of
  insert overhead before the query runs. qj streams: parse row → filter → emit.
- **Fast-path opportunity.** Same trick as NDJSON — for simple filters like `.column`
  or `select(.status == "500")`, extract values directly from parsed CSV without
  building full JSON objects. Skip materialization entirely.
