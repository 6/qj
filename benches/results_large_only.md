# GH Archive Benchmark

> Generated: 2026-02-16 on `Apple M4 Pro (48 GB)`
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

### NDJSON (gharchive.ndjson, 1131MB, parallel processing)

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| ` '.actor.login'` | **75ms** | **97.0x** | 360ms | 20.2x | 7.28s | 2.81s | 6.61s |
| `-c 'length'` | **92ms** | **78.2x** | 592ms | 12.2x | 7.23s | 2.70s | 7.84s |
| `-c 'keys'` | **191ms** | **40.4x** | 734ms | 10.5x | 7.72s | 2.85s | 7.36s |
| `-c 'type'` | **52ms** | **139.4x** | 110ms | 65.5x | 7.22s | 3.44s | 6.67s |
| `-c 'has("actor")'` | **94ms** | **75.8x** | 594ms | 12.0x | 7.14s | 2.72s | 6.63s |
| `-c 'select(.type == "PushEvent")'` | **119ms** | **108.5x** | 430ms | 29.9x | 12.86s | 3.50s | 7.71s |
| `-c 'select(.type == "PushEvent") | .payload.size'` | **81ms** | **88.7x** | 439ms | 16.4x | 7.22s | 2.88s | 6.61s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **128ms** | **61.1x** | 818ms | 9.6x | 7.86s | 3.21s | 6.71s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **379ms** | **20.8x** | 2.25s | 3.5x | 7.87s | 3.10s | 6.83s |
| `-c '{type, commits: (.payload.commits // [] | length)}'` | **352ms** | **21.4x** | 2.02s | 3.7x | 7.55s | 3.07s | 6.76s |

