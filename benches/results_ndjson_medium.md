# GH Archive Benchmark

> Generated: 2026-09-30T04:11:55Z on `Apple M5 Max (128 GB)` (total time: 1191s)
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).
> Tools: qj 0.1.4, jq-1.8.1, jaq 2.3.0, gojq 0.12.18 (rev: fa534a1/go1.25.4)

### NDJSON (gharchive_medium.ndjson, 3.4GB, parallel processing)

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **143.5ms** | **125.7x** | 1.32s | 13.6x | 18.04s | 6.74s | 17.16s |
| `-c 'length'` | **143.6ms** | **126.3x** | 1.35s | 13.4x | 18.14s | 6.37s | 17.56s |
| `-c 'keys'` | **151.7ms** | **130.5x** | 1.49s | 13.3x | 19.79s | 6.93s | 17.76s |
| `-c 'select(.type == "PushEvent")'` | **162.8ms** | **197.6x** | 1.68s | 19.2x | 32.17s | 8.70s | 19.39s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **152.1ms** | **130.4x** | 1.46s | 13.6x | 19.83s | 7.83s | 18.10s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **157.3ms** | **126.2x** | 1.58s | 12.6x | 19.85s | 7.37s | 18.07s |

