// Standalone C++ benchmark — measures pure simdjson throughput without FFI.
//
// qj parses with simdjson's DOM parser (src/simdjson/bridge.cpp's
// jx_tape_parse, wrapped by TapeParser in Rust) and builds jq values from the
// tape. This measures the same DOM parse in C++, so comparing it with
// `cargo bench --bench parse_throughput` ("simdjson DOM parse (FFI, ...)")
// gives the FFI overhead.

#include "simdjson.h"
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <utility>
#include <vector>

using namespace simdjson;
using Clock = std::chrono::high_resolution_clock;

static double mb_per_sec(size_t bytes, double secs) {
    return (double)bytes / (1024.0 * 1024.0) / secs;
}

// Run `work` enough times to fill ~2 seconds, after three warmup runs that
// also calibrate the count.
template <typename F>
static void bench(const char* label, size_t bytes, F work) {
    auto t0 = Clock::now();
    for (int i = 0; i < 3; i++) work();
    double per_iter = std::chrono::duration<double>(Clock::now() - t0).count() / 3.0;
    uint64_t iters = (uint64_t)(2.0 / (per_iter > 1e-9 ? per_iter : 1e-9));
    if (iters < 3) iters = 3;
    if (iters > 1000000) iters = 1000000;

    auto start = Clock::now();
    for (uint64_t i = 0; i < iters; i++) work();
    double secs = std::chrono::duration<double>(Clock::now() - start).count();
    printf("  %-40s %8.1f MB/s  (%llu iters in %.2fs)\n", label,
           mb_per_sec(bytes * iters, secs), (unsigned long long)iters, secs);
}

int main(int argc, char** argv) {
    const char* data_dir = "benches/data";
    if (argc > 1) data_dir = argv[1];

    printf("=== C++ simdjson benchmark (no FFI) ===\n\n");

    // Single-document benchmarks: one DOM parse of the whole file.
    const char* files[] = {"twitter.json"};
    for (auto fname : files) {
        std::string path = std::string(data_dir) + "/" + fname;
        padded_string data;
        auto err = padded_string::load(path).get(data);
        if (err) {
            printf("%-40s SKIPPED (file not found)\n", fname);
            continue;
        }
        printf("%s (%zu bytes):\n", fname, data.size());
        dom::parser parser;
        bench("DOM parse", data.size(), [&] {
            dom::element root;
            if (parser.parse(data).get(root)) abort();
        });
        printf("\n");
    }

    // NDJSON benchmarks: one DOM parse per line with a reused parser, as
    // qj's reader does (each line is followed by readable bytes: the next
    // lines, then the padding).
    const char* ndjson_files[] = {"gharchive.ndjson"};
    for (auto fname : ndjson_files) {
        std::string path = std::string(data_dir) + "/" + fname;
        padded_string data;
        auto err = padded_string::load(path).get(data);
        if (err) {
            printf("%-40s SKIPPED (file not found)\n", fname);
            continue;
        }
        std::vector<std::pair<size_t, size_t>> lines;
        size_t start = 0;
        for (size_t i = 0; i <= data.size(); i++) {
            if (i == data.size() || data.data()[i] == '\n') {
                if (i > start) lines.emplace_back(start, i - start);
                start = i + 1;
            }
        }
        printf("%s (%zu bytes, %zu lines):\n", fname, data.size(), lines.size());
        dom::parser parser;
        const uint8_t* base = reinterpret_cast<const uint8_t*>(data.data());
        bench("DOM parse per line", data.size(), [&] {
            for (auto& [off, len] : lines) {
                dom::element root;
                if (parser.parse(base + off, len, false).get(root)) abort();
            }
        });
        printf("\n");
    }

    return 0;
}
