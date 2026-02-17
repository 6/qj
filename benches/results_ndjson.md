# GH Archive Benchmark

> Generated: 2026-02-17T03:02:25Z on `Apple M4 Pro (48 GB)` (total time: 808s)
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

### NDJSON (gharchive.ndjson, 1130MB, parallel processing)

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **130.3ms** | **55.4x** | 344.7ms | 20.9x | 7.22s | 2.77s | 6.59s |
| `-c 'length'` | **164.0ms** | **43.0x** | 585.5ms | 12.0x | 7.05s | 2.63s | 6.53s |
| `-c 'keys'` | **185.2ms** | **41.5x** | 720.5ms | 10.7x | 7.69s | 2.89s | 6.81s |
| `-c 'type'` | **86.2ms** | **82.5x** | 91.3ms | 77.9x | 7.11s | 3.13s | 6.52s |
| `-c 'has("actor")'` | **166.1ms** | **42.2x** | 550.5ms | 12.7x | 7.02s | 2.68s | 6.61s |
| `-c 'select(.type == "PushEvent")'` | **133.0ms** | **95.8x** | 348.1ms | 36.6x | 12.73s | 3.48s | 7.65s |
| `-c 'select(.type == "PushEvent") | .payload.size'` | **137.4ms** | **52.1x** | 414.9ms | 17.3x | 7.16s | 2.85s | 6.55s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **189.7ms** | **41.1x** | 754.1ms | 10.3x | 7.79s | 3.20s | 6.79s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **363.2ms** | **21.7x** | 1.62s | 4.9x | 7.88s | 3.09s | 7.02s |
| `-c '{type, commits: (.payload.commits // [] | length)}'` | **358.6ms** | **21.2x** | 1.53s | 5.0x | 7.62s | 3.09s | 6.82s |

