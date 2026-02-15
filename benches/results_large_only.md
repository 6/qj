# Large GH Archive Benchmark

> Generated: 2026-02-15 on `Apple M4 Pro (48 GB)`
> 2 runs, no warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

## Tier: ~1GB

### NDJSON (gharchive.ndjson, 1131MB, parallel processing)

| Filter | **qj** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-c '.'` | **796ms** | **34.7x** | 27.64s | 4.96s | 5.6x | 10.10s | 2.7x |
| `-c 'length'` | **491ms** | **14.4x** | 7.06s | 2.66s | 2.7x | 6.63s | 1.1x |
| `-c 'select(.type == "PushEvent")'` | **783ms** | **17.3x** | 13.56s | 3.67s | 3.7x | 7.93s | 1.7x |
| `-c 'select(.type == "PushEvent") | {actor: .actor.login, commits: (.payload.commits // [] | length)}'` | **2.84s** | **2.7x** | 7.57s | 3.08s | 2.5x | 6.82s | 1.1x |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **505ms** | **15.5x** | 7.84s | 3.22s | 2.4x | 6.91s | 1.1x |

### Throughput (`-c '.'`, single pass)

| File | qj | jq | jaq | gojq |
|------|------:|------:|------:|------:|
| gharchive.ndjson | **1.4 GB/s** | 41 MB/s | 228 MB/s | 112 MB/s |

## Tier: ~5GB

### NDJSON (gharchive_large.ndjson, 4808MB, parallel processing)

| Filter | **qj** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-c '.'` | **5.15s** | **23.1x** | 118.85s | 23.13s | 5.1x | 46.56s | 2.6x |
| `-c 'length'` | **2.48s** | **12.8x** | 31.72s | 12.55s | 2.5x | 31.30s | 1.0x |
| `-c 'select(.type == "PushEvent")'` | **2.94s** | **20.0x** | 58.91s | 17.49s | 3.4x | 36.95s | 1.6x |
| `-c 'select(.type == "PushEvent") | {actor: .actor.login, commits: (.payload.commits // [] | length)}'` | **13.28s** | **2.6x** | 34.25s | 16.66s | 2.1x | 32.85s | 1.0x |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **2.45s** | **15.5x** | 37.99s | 17.63s | 2.2x | 34.80s | 1.1x |

### Throughput (`-c '.'`, single pass)

| File | qj | jq | jaq | gojq |
|------|------:|------:|------:|------:|
| gharchive_large.ndjson | **934 MB/s** | 40 MB/s | 208 MB/s | 103 MB/s |

