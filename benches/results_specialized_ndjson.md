# Specialized NDJSON Tools Comparison

> Dataset: `gharchive_large.ndjson` (4.7GB, ~1.7M records)
> Machine: Apple M4 Pro (48 GB)
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine). Median shown.

**Tools compared:**

| Tool | Description | Approach |
|------|-------------|----------|
| [qj](https://github.com/peterallenwebb/qj) | Full jq-compatible JSON processor | SIMD parsing (simdjson), parallel chunk processing, raw byte-scan fast paths |
| [zog](https://github.com/aikoschurmann/zog) | Purpose-built NDJSON search engine | Zero-allocation SIMD byte scanning, no JSON parsing |
| [ripgrep](https://github.com/BurntSushi/ripgrep) | General-purpose regex search | Optimized regex engine, mmap I/O |

zog and ripgrep are single-threaded. qj uses all cores by default; "1 thread" variant shown for apples-to-apples comparison.

All commands write to stdout (piped to `/dev/null` by hyperfine). Times include I/O.

---

### String equality — common match (~65% of lines)

| Tool | Command | Time |
|------|---------|-----:|
| **qj** | `qj -c 'select(.type == "PushEvent")' data.ndjson` | **248ms** |
| qj (1 thread) | `qj --threads 1 -c 'select(.type == "PushEvent")' data.ndjson` | 481ms |
| zog | `zog --file data.ndjson type eq PushEvent` | 416ms |
| ripgrep | `rg '"type":"PushEvent"' data.ndjson` | 636ms |

### String equality — rare match (<0.05% of lines)

| Tool | Command | Time |
|------|---------|-----:|
| **qj** | `qj -c 'select(.type == "PublicEvent")' data.ndjson` | **224ms** |
| qj (1 thread) | `qj --threads 1 -c 'select(.type == "PublicEvent")' data.ndjson` | 458ms |
| zog | `zog --file data.ndjson type eq PublicEvent` | 426ms |
| ripgrep | `rg '"type":"PublicEvent"' data.ndjson` | 455ms |

### Boolean equality (100% match — every record is public)

| Tool | Command | Time |
|------|---------|-----:|
| **qj** | `qj -c 'select(.public == true)' data.ndjson` | **302ms** |
| qj (1 thread) | `qj --threads 1 -c 'select(.public == true)' data.ndjson` | 782ms |
| zog | `zog --file data.ndjson public eq b:true` | 616ms |
| ripgrep | `rg '"public":true' data.ndjson` | 907ms |

### Not-equal

| Tool | Command | Time |
|------|---------|-----:|
| **qj** | `qj -c 'select(.type != "PushEvent")' data.ndjson` | **284ms** |
| qj (1 thread) | `qj --threads 1 -c 'select(.type != "PushEvent")' data.ndjson` | 457ms |
| zog | `zog --file data.ndjson type neq PushEvent` | 342ms |
| ripgrep | `rg -v '"type":"PushEvent"' data.ndjson` | 718ms |

### Compound AND

| Tool | Command | Time |
|------|---------|-----:|
| **qj** | `qj -c 'select(.type == "PushEvent" and .public == true)' data.ndjson` | **390ms** |
| qj (1 thread) | `qj --threads 1 -c 'select(.type == "PushEvent" and .public == true)' data.ndjson` | 679ms |
| zog | `zog --file data.ndjson type eq PushEvent AND public eq b:true` | 521ms |
| ripgrep | `rg '"type":"PushEvent"' data.ndjson \| rg '"public":true'` | 765ms |

### Compound OR

| Tool | Command | Time |
|------|---------|-----:|
| **qj** | `qj -c 'select(.type == "PushEvent" or .type == "CreateEvent")' data.ndjson` | **258ms** |
| qj (1 thread) | `qj --threads 1 -c 'select(.type == "PushEvent" or .type == "CreateEvent")' data.ndjson` | 575ms |
| zog | `zog --file data.ndjson type eq PushEvent OR type eq CreateEvent` | 591ms |
| ripgrep | `rg '"type":"(Push\|Create)Event"' data.ndjson` | 719ms |

### Aggregation — count matching records

| Tool | Command | Time |
|------|---------|-----:|
| qj | `qj -c 'select(.type == "PushEvent")' data.ndjson \| wc -l` | 1.56s |
| qj (1 thread) | `qj --threads 1 -c 'select(.type == "PushEvent")' data.ndjson \| wc -l` | 1.79s |
| **zog** | `zog --file data.ndjson SELECT count:type WHERE type eq PushEvent` | **537ms** |
| ripgrep | `rg '"type":"PushEvent"' data.ndjson \| wc -l` | 1.45s |

### Substring match

| Tool | Command | Time |
|------|---------|-----:|
| **qj** | `qj -c 'select(.type \| contains("Push"))' data.ndjson` | **448ms** |
| qj (1 thread) | `qj --threads 1 -c 'select(.type \| contains("Push"))' data.ndjson` | 1.87s |
| zog | `zog --file data.ndjson type has Push` | 458ms |
| ripgrep | `rg '"type":"Push' data.ndjson` | 609ms |

### Nested field extraction

| Tool | Command | Time |
|------|---------|-----:|
| **qj** | `qj '.actor.login' data.ndjson` | **563ms** |
| qj (1 thread) | `qj --threads 1 '.actor.login' data.ndjson` | 1.43s |
| zog | - (no nested field support) | - |
| ripgrep | `rg -o '"login":"[^"]*"' data.ndjson` | 1.85s |

---

### Notes

- **zog** wins or is competitive single-threaded — its zero-allocation byte scanner is fast for any field position.
- **ripgrep** is a good baseline but consistently slower than both purpose-built tools on structured queries.
- zog and ripgrep are single-threaded. qj's single-thread times show the raw per-core performance; parallelism accounts for the rest.
