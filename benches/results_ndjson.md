# GH Archive Benchmark

> Generated: 2026-02-17T03:57:11Z on `Apple M4 Pro (48 GB)` (total time: 788s)
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

### NDJSON (gharchive.ndjson, 1130MB, parallel processing)

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **100.7ms** | **71.2x** | 571.7ms | 12.6x | 7.18s | 2.76s | 6.72s |
| `-c 'length'` | **130.9ms** | **54.2x** | 819.2ms | 8.7x | 7.09s | 2.68s | 6.71s |
| `-c 'keys'` | **149.4ms** | **52.1x** | 952.8ms | 8.2x | 7.78s | 2.90s | 6.83s |
| `-c 'type'` | **84.0ms** | **86.1x** | 384.2ms | 18.8x | 7.23s | 3.17s | 6.54s |
| `-c 'has("actor")'` | **96.4ms** | **73.6x** | 961.5ms | 7.4x | 7.10s | 2.60s | 6.63s |
| `-c 'select(.type == "PushEvent")'` | **108.7ms** | **117.6x** | 598.5ms | 21.4x | 12.78s | 3.47s | 7.64s |
| `-c 'select(.type == "PushEvent") | .payload.size'` | **110.8ms** | **64.8x** | 651.3ms | 11.0x | 7.18s | 2.92s | 7.00s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **145.5ms** | **54.9x** | 1.01s | 7.9x | 8.00s | 3.49s | 6.90s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **267.8ms** | **29.4x** | 1.65s | 4.8x | 7.88s | 3.11s | 6.98s |
| `-c '{type, commits: (.payload.commits // [] | length)}'` | **257.3ms** | **29.3x** | 1.54s | 4.9x | 7.53s | 3.03s | 6.77s |

