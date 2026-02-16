# Feedback & Positioning Notes

## Does this project make sense given Parquet/DuckDB/etc.?

### Where Parquet/DuckDB/etc. are eating this lunch

- If someone has a 6GB NDJSON file and wants to run analytical queries repeatedly, converting to Parquet once and using DuckDB/Polars/ClickHouse Local is objectively better — columnar storage, predicate pushdown, orders of magnitude faster for aggregations.
- DuckDB can even read NDJSON directly (`SELECT * FROM 'file.ndjson' WHERE ...`), so the "but I don't want to convert" friction is shrinking.
- The data engineering world is trending hard toward columnar formats. Large-scale NDJSON as a query target is increasingly a transitional state, not a destination.

### Where qj still makes sense

- **Ad-hoc shell pipelines.** `curl api | qj '.results[].name'` — you're not going to fire up DuckDB for that. jq syntax is concise and people know it.
- **Streaming / ephemeral data.** Tailing logs, processing Kafka output, CI pipelines — data that flows through and doesn't sit on disk.
- **One-shot exploration.** You get handed a JSON file and want to poke at it. The cognitive overhead of jq is lower than SQL for simple field extraction and transformation.
- **Environments where jq is already embedded.** Thousands of shell scripts, Makefiles, and CI configs use jq. A drop-in replacement that's faster is a real value prop with zero migration cost.

### Realistic framing

The "large NDJSON benchmark" angle (6GB GH Archive, etc.) is impressive for marketing but is probably not the core use case. The real value is being a better jq for the millions of small-to-medium JSON tasks people already use jq for, where the alternative isn't Parquet — it's just jq being slow or annoying.

If positioned primarily as "process huge NDJSON files faster," that's the weakest pitch because those users should probably switch formats. If positioned as "jq but actually fast, with SIMD parsing and parallelism as a bonus," that's more durable.

For repeated analytical queries on large datasets, DuckDB + Parquet is the right tool — qj's NDJSON speed is a benefit for streaming/one-shot scenarios rather than the primary selling point.
