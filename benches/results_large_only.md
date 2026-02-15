# Large GH Archive Benchmark

> Generated: 2026-02-15 on `Apple M4 Pro (48 GB)`
> 2 runs, no warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

## Tier: ~1GB

### NDJSON (gharchive.ndjson, 1131MB, parallel processing)

| Filter | **qj** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-c '.'` | **779ms** | **35.8x** | 27.92s | 4.91s | 5.7x | 9.91s | 2.8x |
| `-c '.type'` | **488ms** | **14.7x** | 7.15s | 2.82s | 2.5x | 6.62s | 1.1x |
| `-c 'select(.type == "PushEvent")'` | **550ms** | **23.1x** | 12.70s | 3.48s | 3.6x | 7.61s | 1.7x |
| `-c 'select(.type == "PushEvent") | {actor: .actor.login, commits: (.payload.commits // [] | length)}'` | **2.76s** | **2.7x** | 7.46s | 3.07s | 2.4x | 6.75s | 1.1x |

### Throughput (`-c '.'`, single pass)

| File | qj | jq | jaq | gojq |
|------|------:|------:|------:|------:|
| gharchive.ndjson | **1.4 GB/s** | 41 MB/s | 231 MB/s | 114 MB/s |

## Tier: ~5GB

### NDJSON (gharchive_large.ndjson, 4808MB, parallel processing)

| Filter | **qj** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-c '.'` | **3.63s** | **32.4x** | 117.65s | 22.29s | 5.3x | 46.00s | 2.6x |
| `-c '.type'` | **2.24s** | **14.2x** | 31.80s | 12.95s | 2.5x | 30.74s | 1.0x |
| `-c 'select(.type == "PushEvent")'` | **2.59s** | **22.5x** | 58.36s | 16.93s | 3.4x | 36.53s | 1.6x |
| `-c 'select(.type == "PushEvent") | {actor: .actor.login, commits: (.payload.commits // [] | length)}'` | **13.46s** | **2.6x** | 34.41s | 16.96s | 2.0x | 32.80s | 1.0x |

### Throughput (`-c '.'`, single pass)

| File | qj | jq | jaq | gojq |
|------|------:|------:|------:|------:|
| gharchive_large.ndjson | **1.3 GB/s** | 41 MB/s | 216 MB/s | 105 MB/s |

