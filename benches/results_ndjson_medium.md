# GH Archive Benchmark

> Generated: 2026-02-17T06:05:44Z on `Apple M4 Pro (48 GB)` (total time: 1216s)
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

### NDJSON (gharchive_medium.ndjson, 3.4GB, parallel processing)

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **187.1ms** | **115.7x** | 1.02s | 21.3x | 21.65s | 8.36s | 20.00s |
| `-c 'length'` | **191.5ms** | **191.1x** | 1.00s | 36.5x | 36.60s | 10.25s | 22.97s |
| `-c 'keys'` | **333.9ms** | **70.1x** | 2.29s | 10.2x | 23.42s | 9.63s | 20.70s |
| `-c 'type'` | **822.2ms** | **28.6x** | 4.84s | 4.9x | 23.51s | 9.17s | 20.99s |
| `-c 'has("actor")'` | **776.0ms** | **29.2x** | 4.59s | 4.9x | 22.65s | 8.95s | 20.76s |
| `-c 'select(.type == "PushEvent")'` | **-** | **-** | - | - | - | - | - |
| `-c 'select(.type == "PushEvent") | .payload.size'` | **-** | **-** | - | - | - | - | - |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **-** | **-** | - | - | - | - | - |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **-** | **-** | - | - | - | - | - |
| `-c '{type, commits: (.payload.commits // [] | length)}'` | **-** | **-** | - | - | - | - | - |

