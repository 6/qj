# GH Archive Benchmark

> Generated: 2026-02-17T04:21:22Z on `Apple M4 Pro (48 GB)` (total time: 784s)
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

### NDJSON (gharchive.ndjson, 1130MB, parallel processing)

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **79.3ms** | **91.7x** | 348.6ms | 20.8x | 7.27s | 2.78s | 9.21s |
| `-c 'length'` | **295.6ms** | **24.1x** | 898.3ms | 7.9x | 7.12s | 2.69s | 6.90s |
| `-c 'keys'` | **122.2ms** | **62.5x** | 740.2ms | 10.3x | 7.64s | 2.82s | 6.69s |
| `-c 'type'` | **51.1ms** | **138.6x** | 103.1ms | 68.7x | 7.08s | 3.12s | 6.51s |
| `-c 'has("actor")'` | **93.3ms** | **75.6x** | 569.1ms | 12.4x | 7.05s | 2.67s | 6.62s |
| `-c 'select(.type == "PushEvent")'` | **82.2ms** | **154.5x** | 358.6ms | 35.4x | 12.70s | 3.44s | 7.61s |
| `-c 'select(.type == "PushEvent") | .payload.size'` | **48.0ms** | **154.7x** | 387.5ms | 19.2x | 7.42s | 2.90s | 7.17s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **126.1ms** | **61.8x** | 780.8ms | 10.0x | 7.79s | 3.16s | 6.72s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **273.5ms** | **28.6x** | 1.63s | 4.8x | 7.83s | 3.05s | 6.76s |
| `-c '{type, commits: (.payload.commits // [] | length)}'` | **259.4ms** | **28.8x** | 1.54s | 4.8x | 7.48s | 3.04s | 6.75s |

