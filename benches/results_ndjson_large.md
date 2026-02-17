# GH Archive Benchmark

> Generated: 2026-02-17T18:36:47Z on `Apple M4 Pro (48 GB)` (total time: 609s)
> 1 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

### NDJSON (gharchive_large.ndjson, 4.7GB, parallel processing)

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **335.8ms** | **96.4x** | 1.70s | 19.0x | 32.37s | 13.62s | 31.97s |
| `-c 'select(.type == "PushEvent")'` | **376.4ms** | **156.9x** | 2.06s | 28.6x | 59.06s | 16.74s | 38.27s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **1.39s** | **25.6x** | 7.90s | 4.5x | 35.69s | 20.25s | 37.86s |

