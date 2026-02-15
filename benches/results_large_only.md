# Large GH Archive Benchmark

> Generated: 2026-02-15 on `Apple M4 Pro (48 GB)`
> 2 runs, no warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

## Tier: ~1GB

### NDJSON (gharchive.ndjson, 1131MB, parallel processing)

| Filter | **qj (simdjson)** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-c '.'` | **822ms** | **34.4x** | 28.30s | 5.47s | 5.2x | 10.12s | 2.8x |
| `-c 'select(.type == "PushEvent") | {actor: .actor.login, commits: (.payload.commits // [] | length)}'` | **3.05s** | **2.5x** | 7.51s | 3.12s | 2.4x | 8.33s | 0.9x |

### JSON (gharchive.json, 1131MB, single document)

| Filter | **qj (simdjson)** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-c '.'` | **474ms** | **61.3x** | 29.06s | 5.57s | 5.2x | 9.49s | 3.1x |
| `-c '[.[] | select(.type == "PushEvent")]'` | **3.89s** | **3.5x** | 13.58s | 4.33s | 3.1x | 7.25s | 1.9x |

### Throughput (`-c '.'`, single pass)

| File | qj | jq | jaq | gojq |
|------|------:|------:|------:|------:|
| gharchive.ndjson | **1.3 GB/s** | 40 MB/s | 207 MB/s | 112 MB/s |
| gharchive.json | **2.3 GB/s** | 39 MB/s | 203 MB/s | 119 MB/s |

## Tier: ~5GB

### NDJSON (gharchive_large.ndjson, 4808MB, parallel processing)

| Filter | **qj (simdjson)** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-c '.'` | **3.52s** | **34.3x** | 120.70s | 22.43s | 5.4x | 46.35s | 2.6x |
| `-c 'select(.type == "PushEvent") | {actor: .actor.login, commits: (.payload.commits // [] | length)}'` | **13.82s** | **2.5x** | 34.16s | 16.48s | 2.1x | 32.27s | 1.1x |

### JSON (gharchive_large.json, 4808MB, single document)

| Filter | **qj (serde_json†)** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-c '.'` | **2.90s** | **42.5x** | 123.35s | 24.95s | 4.9x | 38.73s | 3.2x |
| `-c '[.[] | select(.type == "PushEvent")]'` | **22.46s** | **2.8x** | 62.05s | 23.57s | 2.6x | 32.19s | 1.9x |

### Throughput (`-c '.'`, single pass)

| File | qj | jq | jaq | gojq |
|------|------:|------:|------:|------:|
| gharchive_large.ndjson | **1.3 GB/s** | 40 MB/s | 214 MB/s | 104 MB/s |
| gharchive_large.json | **1.6 GB/s** | 39 MB/s | 193 MB/s | 124 MB/s |

†serde_json fallback for >4GB single-document JSON (simdjson 4GB limit)

