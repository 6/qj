# Extended NDJSON Benchmarks

> Generated: 2026-09-30T04:40:30Z on `Apple M5 Max (128 GB)` (total time: 528s)
> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine).
> Dataset: gharchive_xsmall.ndjson (514MB)
> Tools: qj 0.1.4, jq-1.8.1, jaq 2.3.0, gojq 0.12.18 (rev: fa534a1/go1.25.4)

## Streaming (file)

Standard NDJSON filters with mmap + parallelism.

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **28.7ms** | **94.9x** | 201.1ms | 13.6x | 2.73s | 1.02s | 2.61s |
| `-c 'length'` | **28.0ms** | **96.3x** | 202.6ms | 13.3x | 2.70s | 977.6ms | 2.59s |
| `-c 'keys'` | **28.4ms** | **105.0x** | 222.6ms | 13.4x | 2.98s | 1.04s | 2.65s |
| `-c 'select(.type == "PushEvent")'` | **32.8ms** | **168.0x** | 256.3ms | 21.5x | 5.51s | 1.34s | 2.98s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **29.5ms** | **104.4x** | 224.6ms | 13.7x | 3.08s | 1.20s | 2.69s |
| `-c '{type, commits: [.payload.commits[]?.message]}'` | **30.2ms** | **102.1x** | 243.4ms | 12.6x | 3.08s | 1.17s | 2.72s |

## Complex filters

Filters using `def`/`reduce`. Still parallel.

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-c 'def is_push: .type == "PushEvent"; select(is_push)'` | **32.7ms** | **169.2x** | 259.8ms | 21.3x | 5.54s | 1.34s | 3.00s |
| `-c 'reduce .payload.commits[]? as $c (""; . + $c.message[0:1])'` | **49.2ms** | **58.3x** | 477.2ms | 6.0x | 2.87s | 1.13s | 2.71s |

## Stdin (`cat file | tool`)

Piped via stdin instead of file argument. No mmap, may affect parallelism.

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `'.actor.login'` | **177.4ms** | **15.5x** | 266.8ms | 10.3x | 2.74s | 2.11s | 2.68s |
| `-c 'select(.type == "PushEvent")'` | **180.9ms** | **29.4x** | 327.4ms | 16.3x | 5.32s | 2.45s | 3.05s |
| `-c '{type, repo: .repo.name, actor: .actor.login}'` | **179.5ms** | **17.0x** | 293.9ms | 10.4x | 3.05s | 2.30s | 2.78s |

## Slurp mode (`-s`)

All records loaded into array. No parallelism.

| Filter | **qj** | vs jq | qj (1T) | vs jq | jq | jaq | gojq |
|--------|------:|------:|------:|------:|------:|------:|------:|
| `-s 'length'` | **516.3ms** | **5.7x** | 518.3ms | 5.7x | 2.96s | 1.18s | 2.38s |
| `-s 'group_by(.type) | map({type: .[0].type, count: length})'` | **619.6ms** | **5.2x** | 618.2ms | 5.2x | 3.24s | 1.33s | 2.51s |
| `-s 'map(.actor.login) | group_by(.) | map({user: .[0], events: length}) | sort_by(.events) | reverse | .[:10]'` | **627.0ms** | **5.3x** | 631.1ms | 5.2x | 3.30s | 1.34s | 2.64s |

