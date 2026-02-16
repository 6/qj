# Large GH Archive Benchmark

> Generated: 2026-02-15 on `Apple M4 Pro (48 GB)`
> 2 runs, no warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

## Tier: ~1GB

### NDJSON (gharchive.ndjson, 1131MB, parallel processing)

| Filter | **qj** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| ` '.actor.login'` | **69ms** | **104.2x** | 7.21s | 2.72s | 2.6x | 6.65s | 1.1x |
| `-c 'length'` | **88ms** | **79.8x** | 7.03s | 2.67s | 2.6x | 6.61s | 1.1x |
| `-c 'keys'` | **109ms** | **70.6x** | 7.73s | 2.86s | 2.7x | 6.81s | 1.1x |
| `-c 'select(.type == "PushEvent")'` | **102ms** | **124.2x** | 12.66s | 3.47s | 3.7x | 7.73s | 1.6x |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **127ms** | **61.2x** | 7.78s | 3.19s | 2.4x | 6.89s | 1.1x |
| `-c '{type, size: (.payload.size // 0)}'` | **443ms** | **16.8x** | 7.45s | 2.98s | 2.5x | 6.85s | 1.1x |

## Tier: ~5GB

### NDJSON (gharchive_large.ndjson, 4808MB, parallel processing)

| Filter | **qj** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| ` '.actor.login'` | **2.44s** | **13.1x** | 31.98s | 13.00s | 2.5x | 30.99s | 1.0x |
| `-c 'length'` | **415ms** | **74.9x** | 31.12s | 11.93s | 2.6x | 31.00s | 1.0x |
| `-c 'keys'` | **560ms** | **64.6x** | 36.21s | 13.54s | 2.7x | 32.02s | 1.1x |
| `-c 'select(.type == "PushEvent")'` | **526ms** | **110.1x** | 57.99s | 17.07s | 3.4x | 36.97s | 1.6x |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **668ms** | **56.2x** | 37.55s | 16.80s | 2.2x | 33.56s | 1.1x |
| `-c '{type, size: (.payload.size // 0)}'` | **2.34s** | **14.8x** | 34.63s | 14.96s | 2.3x | 32.81s | 1.1x |

