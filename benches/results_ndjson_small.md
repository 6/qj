# GH Archive Benchmark

> Generated: 2026-09-30T04:32:34Z on `Apple M5 Max (128 GB)` (total time: 415s)
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).
> Tools: qj 0.1.4, jq-1.8.1, jaq 2.3.0, gojq 0.12.18 (rev: fa534a1/go1.25.4)

### NDJSON (gharchive.ndjson, 1.1GB, parallel processing)

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **52.7ms** | **111.5x** | 434.1ms | 13.5x | 5.88s | 2.17s | 5.60s |
| `-c 'length'` | **52.2ms** | **113.4x** | 434.9ms | 13.6x | 5.92s | 2.14s | 5.61s |
| `-c 'keys'` | **54.7ms** | **119.6x** | 484.0ms | 13.5x | 6.54s | 2.24s | 5.90s |
| `-c 'select(.type == "PushEvent")'` | **60.1ms** | **188.4x** | 556.2ms | 20.4x | 11.33s | 2.84s | 6.43s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **55.4ms** | **121.0x** | 486.4ms | 13.8x | 6.70s | 2.61s | 5.87s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **57.9ms** | **117.0x** | 525.4ms | 12.9x | 6.78s | 2.51s | 5.93s |

