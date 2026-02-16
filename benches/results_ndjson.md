# GH Archive Benchmark

> Generated: 2026-02-16 on `Apple M4 Pro (48 GB)`
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

### NDJSON (gharchive.ndjson, 1130MB, parallel processing)

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **66.2ms** | **109.3x** | 337.6ms | 21.4x | 7.24s | 2.85s | 7.09s |
| `-c 'length'` | **108.0ms** | **66.4x** | 593.1ms | 12.1x | 7.18s | 2.69s | 7.21s |
| `-c 'keys'` | **109.4ms** | **70.6x** | 736.5ms | 10.5x | 7.73s | 2.90s | 6.77s |
| `-c 'type'` | **47.2ms** | **152.4x** | 106.7ms | 67.5x | 7.20s | 3.18s | 6.62s |
| `-c 'has("actor")'` | **86.3ms** | **83.3x** | 577.4ms | 12.5x | 7.19s | 2.81s | 6.78s |
| `-c 'select(.type == "PushEvent")'` | **100.9ms** | **133.9x** | 405.5ms | 33.3x | 13.51s | 3.48s | 7.94s |
| `-c 'select(.type == "PushEvent") | .payload.size'` | **76.6ms** | **94.7x** | 427.9ms | 17.0x | 7.25s | 2.94s | 6.95s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **115.7ms** | **68.3x** | 778.7ms | 10.1x | 7.90s | 3.25s | 7.16s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **268.0ms** | **29.8x** | 1.72s | 4.6x | 7.99s | 3.17s | 6.95s |
| `-c '{type, commits: (.payload.commits // [] | length)}'` | **261.6ms** | **28.8x** | 1.54s | 4.9x | 7.54s | 3.07s | 6.79s |

