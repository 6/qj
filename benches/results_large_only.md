# GH Archive Benchmark

> Generated: 2026-02-15 on `Apple M4 Pro (48 GB)`
> 2 runs, no warmup via [hyperfine](https://github.com/sharkdp/hyperfine).

### NDJSON (gharchive.ndjson, 1131MB, parallel processing)

| Filter | **qj** | vs jq | jq | jaq | vs jq | gojq | vs jq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| ` '.actor.login'` | **77ms** | **94.2x** | 7.21s | 2.85s | 2.5x | 6.72s | 1.1x |
| `-c 'length'` | **108ms** | **66.4x** | 7.17s | 2.80s | 2.6x | 6.73s | 1.1x |
| `-c 'keys'` | **126ms** | **61.1x** | 7.70s | 2.87s | 2.7x | 6.72s | 1.1x |
| `-c 'select(.type == "PushEvent")'` | **106ms** | **127.4x** | 13.45s | 3.51s | 3.8x | 7.70s | 1.7x |
| `-c 'select(.type == "PushEvent") | .payload.size'` | **80ms** | **91.3x** | 7.26s | 2.89s | 2.5x | 6.96s | 1.0x |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **134ms** | **60.2x** | 8.07s | 3.29s | 2.5x | 6.96s | 1.2x |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **494ms** | **16.0x** | 7.89s | 3.09s | 2.6x | 6.92s | 1.1x |
| `-c '{type, commits: (.payload.commits // [] | length)}'` | **2.73s** | **2.8x** | 7.58s | 3.14s | 2.4x | 6.70s | 1.1x |

