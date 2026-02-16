# GH Archive Benchmark

> Generated: 2026-02-16 on `Apple M4 Pro (48 GB)`
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

### NDJSON (gharchive.ndjson, 1131MB, parallel processing)

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| ` '.actor.login'` | **75ms** | **95.8x** | 347ms | 20.7x | 7.19s | 2.76s | 6.52s |
| `-c 'length'` | **91ms** | **77.2x** | 576ms | 12.2x | 7.03s | 2.66s | 6.58s |
| `-c 'keys'` | **120ms** | **63.8x** | 733ms | 10.4x | 7.64s | 2.83s | 6.78s |
| `-c 'select(.type == "PushEvent")'` | **104ms** | **121.2x** | 405ms | 31.2x | 12.62s | 3.48s | 7.63s |
| `-c 'select(.type == "PushEvent") | .payload.size'` | **78ms** | **91.1x** | 426ms | 16.8x | 7.15s | 2.90s | 6.56s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **132ms** | **59.1x** | 828ms | 9.5x | 7.83s | 3.28s | 6.75s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **295ms** | **26.7x** | 1.78s | 4.4x | 7.87s | 3.11s | 7.03s |
| `-c '{type, commits: (.payload.commits // [] | length)}'` | **266ms** | **28.2x** | 1.56s | 4.8x | 7.49s | 3.08s | 6.85s |

