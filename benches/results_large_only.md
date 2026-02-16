# Large GH Archive Benchmark

> Generated: 2026-02-15 on `Apple M4 Pro (48 GB)`
> 2 runs, no warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

## Tier: ~1GB

### NDJSON (gharchive.ndjson, 1131MB, parallel processing)

| Filter | **qj** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-c '.'` | **772ms** | **35.6x** | 27.49s | 5.02s | 5.5x | 9.93s | 2.8x |
| `-c 'length'` | **101ms** | **71.4x** | 7.20s | 2.74s | 2.6x | 6.71s | 1.1x |
| `-c 'select(.type == "PushEvent")'` | **119ms** | **108.0x** | 12.81s | 3.54s | 3.6x | 7.80s | 1.6x |
| `-c 'select(.type == "PushEvent") | {actor: .actor.login, commits: (.payload.commits // [] | length)}'` | **2.76s** | **2.7x** | 7.54s | 3.12s | 2.4x | 6.93s | 1.1x |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **134ms** | **58.5x** | 7.86s | 3.26s | 2.4x | 6.95s | 1.1x |

### Throughput (`-c '.'`, single pass)

| File | qj | jq | jaq | gojq |
|------|------:|------:|------:|------:|
| gharchive.ndjson | **1.4 GB/s** | 41 MB/s | 225 MB/s | 114 MB/s |

## Tier: ~5GB

### NDJSON (gharchive_large.ndjson, 4808MB, parallel processing)

| Filter | **qj** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-c '.'` | **5.95s** | **19.9x** | 118.25s | 22.69s | 5.2x | 47.45s | 2.5x |
| `-c 'length'` | **481ms** | **65.2x** | 31.37s | 12.84s | 2.4x | 30.96s | 1.0x |
| `-c 'select(.type == "PushEvent")'` | **459ms** | **127.2x** | 58.37s | 17.25s | 3.4x | 36.85s | 1.6x |
| `-c 'select(.type == "PushEvent") | {actor: .actor.login, commits: (.payload.commits // [] | length)}'` | **13.27s** | **2.6x** | 34.50s | 16.72s | 2.1x | 32.37s | 1.1x |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **668ms** | **56.6x** | 37.80s | 17.68s | 2.1x | 33.15s | 1.1x |

### Throughput (`-c '.'`, single pass)

| File | qj | jq | jaq | gojq |
|------|------:|------:|------:|------:|
| gharchive_large.ndjson | **808 MB/s** | 41 MB/s | 212 MB/s | 101 MB/s |

