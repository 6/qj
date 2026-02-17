# Extended NDJSON Benchmarks

> Generated: 2026-02-17T17:48:42Z on `Apple M4 Pro (48 GB)` (total time: 303s)
> 1 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).
> Dataset: gharchive_xsmall.ndjson (514MB)

## Streaming (file)

Standard NDJSON filters with mmap + parallelism + on-demand fast paths.

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **34.7ms** | **94.6x** | 157.5ms | 20.8x | 3.28s | 1.28s | 2.98s |
| `-c 'length'` | **47.5ms** | **69.1x** | 272.7ms | 12.0x | 3.28s | 1.24s | 3.05s |
| `-c 'keys'` | **58.2ms** | **60.4x** | 329.9ms | 10.7x | 3.52s | 1.31s | 3.06s |
| `-c 'select(.type == "PushEvent")'` | **45.8ms** | **131.1x** | 221.6ms | 27.1x | 6.01s | 1.65s | 3.54s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **56.7ms** | **63.7x** | 360.7ms | 10.0x | 3.61s | 1.59s | 3.17s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **129.5ms** | **27.9x** | 759.1ms | 4.8x | 3.62s | 1.43s | 3.19s |

## Complex filters (no on-demand fast path)

Filters using `def`/`reduce` that bypass on-demand extraction. Still parallel.

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-c 'def is_push: .type == "PushEvent"; select(is_push)'` | **211.1ms** | **28.8x** | 1.58s | 3.8x | 6.07s | 1.67s | 3.54s |
| `-c 'reduce .payload.commits[]? as $c (""; . + $c.message[0:1])'` | **138.9ms** | **25.0x** | 815.0ms | 4.3x | 3.47s | 1.42s | 3.18s |

## Stdin (`cat file | tool`)

Piped via stdin instead of file argument. No mmap, may affect parallelism.

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **363.9ms** | **9.1x** | 497.9ms | 6.7x | 3.31s | 2.28s | 3.02s |
| `-c 'select(.type == "PushEvent")'` | **356.7ms** | **16.6x** | 508.9ms | 11.7x | 5.93s | 2.59s | 3.60s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **346.0ms** | **10.3x** | 623.8ms | 5.7x | 3.57s | 2.52s | 3.23s |

## Slurp mode (`-s`)

All records loaded into array. No parallelism or on-demand fast paths.

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-s 'length'` | **1.45s** | **2.5x** | 1.40s | 2.6x | 3.61s | 1.52s | 2.87s |
| `-s 'group_by(.type) | map({type: .[0].type, count: length})'` | **1.43s** | **2.8x** | 1.45s | 2.7x | 3.97s | 1.68s | 3.05s |
| `-s 'map(.actor.login) | group_by(.) | map({user: .[0], events: length}) | sort_by(.events) | reverse | .[:10]'` | **1.48s** | **2.7x** | 1.44s | 2.8x | 4.06s | 1.78s | 3.26s |

