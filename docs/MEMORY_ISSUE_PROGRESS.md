# NDJSON Memory vs Throughput Investigation

## Problem

The streaming NDJSON changes (commit `6d8a3a1`, "Streaming NDJSON: reduce peak RSS from 2.6 GB to 109 MB") caused a **1.8x throughput regression** on the 1.1 GB gharchive benchmark:

| Metric | Before streaming | After streaming |
|--------|---:|---:|
| `.actor.login` | 67 ms | 130 ms |
| `select(.type == "PushEvent")` | ~101 ms | ~133 ms |
| Peak RSS | ~2.6 GB | ~109 MB |

The README benchmark table was never updated, so it showed the pre-streaming 67 ms numbers while the binary actually ran at 130 ms.

## Root cause

The pre-streaming approach used `read_padded_file` (mmap) to map the entire file, then `process_ndjson()` split it into ~1 MB chunks and processed **all ~1100 chunks in a single `par_iter`** call. Maximum parallelism.

The streaming approach (`process_ndjson_streaming`) reads the file through `read()` syscalls in fixed-size windows (8-64 MB, scaled to core count). Each window's chunks are processed in parallel, then the next window is read. On a 10-core M4 Pro the default window was 20 MB, meaning **~55 sequential windows** with rayon synchronization overhead between each.

The throughput loss came from:
1. `read()` syscall overhead vs mmap zero-copy
2. Sequential window boundaries limiting parallelism
3. Rayon thread pool idle time between windows

## Approaches tried

### 1. Revert to all-at-once mmap (reverted)

Restored the original `read_padded_file` + `process_ndjson()` path for files.

- **Speed**: 69 ms (full recovery)
- **Memory**: ~1.2 GB RSS on 1.1 GB file (mmap pages + full output buffer)
- **Problem**: `process_ndjson` buffers ALL output in a single `Vec<u8>` before writing. For large files this means unbounded output memory. Also, `read_padded_file` falls back to `vec![0u8; file_size]` when mmap can't provide simdjson padding (~1.5% of files by size alignment) — this would OOM on files larger than RAM.

### 2. mmap + windowed processing with MADV_DONTNEED (reverted)

Used mmap for input but processed through windows, calling `madvise(MADV_DONTNEED)` after each window to release processed pages.

- **Speed**: 83 ms (14 ms overhead from madvise syscalls)
- **Memory**: 1175 MB RSS (MADV_DONTNEED is a weak hint on macOS for file-backed mappings — the kernel doesn't immediately free pages)
- **Reverted** because the syscall overhead cost speed without measurably reducing RSS on macOS.

### 3. Window size tuning

Tested different window sizes with mmap + windowed processing (no MADV_DONTNEED):

| Window | Time | vs all-at-once (69 ms) |
|--------|-----:|---:|
| 20 MB (default) | 86 ms | +25% |
| 64 MB | 77 ms | +12% |
| 128 MB | 72 ms | +5% |
| 256 MB | 70 ms | +1% |
| 512 MB | 68 ms | ~same |

Diminishing returns after 256 MB. At 256 MB the overhead is within noise of all-at-once.

### 4. Final design: dedicated mmap + windowed (current)

**`process_ndjson_file()`** — unified entry point for NDJSON file processing:

1. **mmap directly** (no simdjson padding needed for NDJSON — lines are parsed individually)
2. Detect NDJSON from the mmap'd buffer
3. Process through **`process_ndjson_windowed()`** with 256 MB windows
4. If mmap fails or is unavailable (non-Unix, `QJ_NO_MMAP=1`): fall back to `process_ndjson_streaming()` with `read()` and conservative 8-64 MB windows

Key design choices:
- **mmap window: 256 MB fixed** — input memory is kernel-managed virtual memory (not heap), so only output is buffered per window. 256 MB gives ~256 chunks, plenty for any core count.
- **Streaming window: `num_cores × 2 MB`, clamped 8-64 MB** — the window IS the heap buffer, so we cap conservatively.
- **No `read_padded_file` for NDJSON** — avoids the heap fallback that would OOM on files >> RAM.
- **`MADV_SEQUENTIAL`** on the mmap to hint read-ahead and evict-behind.

Results:
- **Speed**: 68 ms (matches pre-streaming performance)
- **RSS**: ~1.2 GB on 1.1 GB file (mmap'd pages, kernel-managed)
- **Files >> RAM**: Works correctly. mmap is virtual address space, not physical memory. Windowed access means only ~256 MB of pages are actively touched. The kernel pages in on demand and evicts clean file-backed pages under pressure. This is how databases handle files larger than RAM.

## RSS vs actual memory pressure

The ~1.2 GB RSS for a 1.1 GB file looks high but is misleading for mmap:
- These are **clean, file-backed pages** — the kernel evicts them instantly when anything else needs memory (no writeback needed)
- Under memory pressure, the working set is just the current 256 MB window + output buffer
- `MADV_SEQUENTIAL` tells the kernel to optimize for forward access
- Tools like `/usr/bin/time -l` report peak RSS including mmap'd pages, which overstates actual memory pressure

## Memory profile by path

| Input source | Method | Input memory | Output memory | Peak working set |
|---|---|---|---|---|
| File (Unix) | mmap + windowed | Kernel-managed (virtual) | ~window output | ~256 MB + output |
| File (non-Unix) | streaming read() | window heap buffer | ~window output | ~64 MB + output |
| stdin/pipe | streaming read() | window heap buffer | ~window output | ~64 MB + output |
| Compressed file | decompress + streaming | window heap buffer | ~window output | ~64 MB + output |

## Open questions

- Should we add `MADV_DONTNEED` after each window on Linux (where it's stronger than macOS)?
- Should the mmap window size be configurable separately from the streaming window size via env var? Currently `QJ_WINDOW_SIZE` overrides both.
- For extremely large output (e.g., `.` on a 24 GB file = 24 GB output), per-window output buffering still means ~256 MB of output buffered at a time. Could flush more aggressively per-chunk, but that would require changing the rayon collect pattern.
