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

### 4. Full-file mmap + windowed (reverted — RSS too high)

**`process_ndjson_file()`** — unified entry point for NDJSON file processing:

1. **mmap directly** (no simdjson padding needed for NDJSON — lines are parsed individually)
2. Detect NDJSON from the mmap'd buffer
3. Process through **`process_ndjson_windowed()`** with 256 MB windows
4. If mmap fails or is unavailable (non-Unix, `QJ_NO_MMAP=1`): fall back to `process_ndjson_streaming()` with `read()` and conservative 8-64 MB windows

Results:
- **Speed**: 68 ms (matches pre-streaming performance)
- **RSS**: ~1.2 GB on 1.1 GB file (mmap'd pages, kernel-managed)
- **Heap**: ~43 MB (actual allocation)
- **Reverted**: 1.2 GB RSS on a 1.1 GB file is too high for a CLI tool, even though the pages are clean file-backed memory. Users see the RSS number and think the tool uses that much memory.

### 5. Per-window mmap (reverted — too slow)

Map each 256 MB window independently via `mmap(fd, offset, len)`, process it, then `munmap` before mapping the next window. Zero-copy I/O with RSS bounded to ~window_size.

Results with different window sizes on 1.1 GB file:

| Window | Time | RSS |
|--------|-----:|----:|
| 64 MB | 190 ms | 111 MB |
| 128 MB | 120 ms | 171 MB |
| 256 MB | 100 ms | 299 MB |
| 512 MB | 80 ms | 556 MB |

- **Reverted**: 100ms at 256 MB is a 47% regression vs full-file mmap (70ms). The overhead comes from losing kernel read-ahead context between windows — each `mmap_window` starts cold, unlike a single MADV_SEQUENTIAL mmap that gives the kernel full file context.

### 6. Full-file mmap + progressive munmap (current)

Combines the speed of full-file mmap with the RSS control of per-window mmap:

1. **mmap the entire file** in one call with `MADV_SEQUENTIAL` — kernel has full context for aggressive read-ahead
2. **Process in 128 MB windows** with rayon parallelism
3. **`munmap` each processed region** (page-aligned) before moving to the next window — releases pages to bound RSS

This avoids the two problems of previous approaches:
- Full-file mmap had ~1.2 GB RSS because all pages stayed mapped
- Per-window mmap was slow (100ms) because each window lost read-ahead context

With progressive munmap, the single mmap + MADV_SEQUENTIAL gives the kernel full read-ahead context (fast), while the per-window munmap releases processed pages (low RSS).

Window size testing (progressive munmap, 1.1 GB gharchive):

| Window | Time | RSS |
|--------|-----:|----:|
| 64 MB | 80 ms | 107 MB |
| 128 MB | 70 ms | 172 MB |
| 256 MB | 70 ms | 301 MB |
| 512 MB | 70 ms | 556 MB |

128 MB is the smallest window that matches full speed. Larger windows only increase RSS
without improving throughput, so the cap is set to 128 MB. Users can override via
`QJ_WINDOW_SIZE=256` if desired. For very large files (20 GB+), the extra rayon barriers
from more windows add ~microseconds each — negligible.

Results:
- **Speed**: 70 ms (matches pre-streaming and full-file mmap)
- **RSS**: ~174 MB on 1.1 GB file (current window + read-ahead)
- **Heap**: ~42 MB (actual allocation)
- **Files >> RAM**: Works correctly — kernel pages in on demand, and progressive munmap ensures only the current window's pages are resident.

## Memory profile by path

| Input source | Method | Input memory | Output memory | Peak RSS |
|---|---|---|---|---|
| File (Unix) | mmap + progressive munmap | Kernel-managed | ~window output | ~174 MB |
| File (non-Unix) | streaming read() | window heap buffer | ~window output | ~128 MB |
| stdin/pipe | streaming read() | window heap buffer | ~window output | ~128 MB |
| Compressed file | decompress + streaming | window heap buffer | ~window output | ~128 MB |

## Open questions

- Should we add `MADV_DONTNEED` after each window on Linux (where it's stronger than macOS) as an alternative to munmap?
- For extremely large output (e.g., `.` on a 24 GB file = 24 GB output), per-window output buffering still means ~256 MB of output buffered at a time. Could flush more aggressively per-chunk, but that would require changing the rayon collect pattern.
