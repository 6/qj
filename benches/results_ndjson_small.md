# GH Archive Benchmark

> Generated: 2026-02-17T04:51:19Z on `Apple M4 Pro (48 GB)` (total time: 806s)
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

### NDJSON (gharchive.ndjson, 1130MB, parallel processing)

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **75.9ms** | **96.4x** | 354.6ms | 20.6x | 7.32s | 2.75s | 6.65s |
| `-c 'length'` | **101.2ms** | **70.3x** | 589.5ms | 12.1x | 7.11s | 2.87s | 7.17s |
| `-c 'keys'` | **122.9ms** | **62.4x** | 739.9ms | 10.4x | 7.67s | 2.81s | 6.62s |
| `-c 'type'` | **52.5ms** | **136.0x** | 102.6ms | 69.7x | 7.15s | 3.15s | 6.57s |
| `-c 'has("actor")'` | **95.5ms** | **73.7x** | 560.1ms | 12.6x | 7.04s | 2.64s | 6.56s |
| `-c 'select(.type == "PushEvent")'` | **76.4ms** | **166.3x** | 345.5ms | 36.8x | 12.70s | 3.46s | 7.49s |
| `-c 'select(.type == "PushEvent") | .payload.size'` | **79.8ms** | **89.4x** | 426.0ms | 16.7x | 7.13s | 2.91s | 6.60s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **127.7ms** | **61.1x** | 751.4ms | 10.4x | 7.80s | 3.21s | 6.74s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **269.9ms** | **29.1x** | 1.63s | 4.8x | 7.84s | 3.06s | 6.83s |
| `-c '{type, commits: (.payload.commits // [] | length)}'` | **263.5ms** | **28.5x** | 1.54s | 4.9x | 7.50s | 3.02s | 6.76s |

