# Large GH Archive Benchmark

> Generated: 2026-02-15 on `Apple M4 Pro (48 GB)`
> 2 runs, no warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

## Tier: ~1GB

### NDJSON (gharchive.ndjson, 1131MB, parallel processing)

| Filter | **qj** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| ` '.actor.login'` | **404ms** | **17.9x** | 7.22s | 2.87s | 2.5x | 6.69s | 1.1x |
| `-c 'length'` | **116ms** | **61.1x** | 7.09s | 2.71s | 2.6x | 6.68s | 1.1x |
| `-c 'select(.type == "PushEvent")'` | **112ms** | **114.2x** | 12.85s | 3.48s | 3.7x | 7.66s | 1.7x |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **140ms** | **55.8x** | 7.81s | 3.19s | 2.4x | 6.92s | 1.1x |
| `-c 'select(.type == "PushEvent") | {login: .actor.login, commits: (.payload.commits // [] | length)}'` | **2.72s** | **2.8x** | 7.49s | 3.08s | 2.4x | 6.76s | 1.1x |

### Throughput (`-c '.'`, single pass)

| File | qj | jq | jaq | gojq |
|------|------:|------:|------:|------:|
| gharchive.ndjson | **2.7 GB/s** | 157 MB/s | 395 MB/s | 169 MB/s |

## Tier: ~5GB

### NDJSON (gharchive_large.ndjson, 4808MB, parallel processing)

| Filter | **qj** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| ` '.actor.login'` | **328ms** | **99.3x** | 32.57s | 13.23s | 2.5x | 30.81s | 1.1x |
| `-c 'length'` | **485ms** | **65.7x** | 31.85s | 12.07s | 2.6x | 30.45s | 1.0x |
| `-c 'select(.type == "PushEvent")'` | **527ms** | **111.0x** | 58.49s | 17.83s | 3.3x | 36.41s | 1.6x |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **662ms** | **58.7x** | 38.84s | 17.27s | 2.2x | 33.00s | 1.2x |
| `-c 'select(.type == "PushEvent") | {login: .actor.login, commits: (.payload.commits // [] | length)}'` | **13.19s** | **2.6x** | 34.49s | 16.65s | 2.1x | 32.16s | 1.1x |

### Throughput (`-c '.'`, single pass)

| File | qj | jq | jaq | gojq |
|------|------:|------:|------:|------:|
| gharchive_large.ndjson | **14.3 GB/s** | 148 MB/s | 363 MB/s | 156 MB/s |

