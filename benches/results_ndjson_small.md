# GH Archive Benchmark

> Generated: 2026-02-17T15:09:08Z on `Apple M4 Pro (48 GB)` (total time: 484s)
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

### NDJSON (gharchive.ndjson, 1.1GB, parallel processing)

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **76.8ms** | **95.1x** | 358.0ms | 20.4x | 7.31s | 2.77s | 6.66s |
| `-c 'length'` | **100.2ms** | **71.4x** | 596.7ms | 12.0x | 7.15s | 2.67s | 6.53s |
| `-c 'keys'` | **121.9ms** | **63.4x** | 755.5ms | 10.2x | 7.73s | 2.81s | 6.71s |
| `-c 'select(.type == "PushEvent")'` | **93.6ms** | **136.5x** | 420.8ms | 30.4x | 12.78s | 3.48s | 7.69s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **133.4ms** | **59.1x** | 775.0ms | 10.2x | 7.89s | 3.24s | 6.98s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **273.4ms** | **29.0x** | 1.66s | 4.8x | 7.94s | 3.11s | 6.87s |

