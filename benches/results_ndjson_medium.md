# GH Archive Benchmark

> Generated: 2026-02-17T06:35:32Z on `Apple M4 Pro (48 GB)` (total time: 977s)
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

### NDJSON (gharchive_medium.ndjson, 3.4GB, parallel processing)

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **196.1ms** | **110.7x** | 1.02s | 21.4x | 21.72s | 8.18s | 20.26s |
| `-c 'select(.type == "PushEvent")'` | **189.8ms** | **191.6x** | 1.03s | 35.4x | 36.36s | 10.36s | 22.84s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **331.9ms** | **70.4x** | 2.26s | 10.3x | 23.36s | 9.51s | 20.69s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **801.2ms** | **29.7x** | 4.84s | 4.9x | 23.82s | 9.20s | 20.91s |

