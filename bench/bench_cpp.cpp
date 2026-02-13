// Standalone C++ benchmark — measures pure simdjson throughput without FFI.
// Used to calculate FFI overhead by comparing with the Rust benchmark.

#include "simdjson.h"
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <string>
#include <vector>

using namespace simdjson;
using Clock = std::chrono::high_resolution_clock;

static std::string read_file(const char* path) {
    std::ifstream f(path, std::ios::binary | std::ios::ate);
    if (!f) {
        fprintf(stderr, "Cannot open %s\n", path);
        exit(1);
    }
    auto sz = f.tellg();
    std::string buf(sz, '\0');
    f.seekg(0);
    f.read(buf.data(), sz);
    return buf;
}

static double mb_per_sec(size_t bytes, double secs) {
    return (double)bytes / (1024.0 * 1024.0) / secs;
}

// Auto-calibrate: run enough iterations to fill ~2 seconds.
static uint64_t calibrate(size_t bytes) {
    // Target ~2s. Estimate ~2 GB/s throughput for calibration.
    uint64_t iters = (uint64_t)(2.0 * 2e9 / (double)bytes);
    if (iters < 10) iters = 10;
    return iters;
}

static void bench_ondemand_parse(const char* label, const padded_string& data) {
    uint64_t iters = calibrate(data.size());
    ondemand::parser parser;

    // Warmup
    for (uint64_t i = 0; i < 3; i++) {
        auto doc = parser.iterate(data);
        (void)doc;
    }

    auto start = Clock::now();
    for (uint64_t i = 0; i < iters; i++) {
        auto doc = parser.iterate(data);
        (void)doc;
    }
    auto end = Clock::now();
    double secs = std::chrono::duration<double>(end - start).count();
    double mbs = mb_per_sec(data.size() * iters, secs);
    printf("  %-35s %8.1f MB/s  (%llu iters in %.2fs)\n", label, mbs, iters, secs);
}

static void bench_ondemand_field(const char* label, const padded_string& data,
                                  const char* field) {
    uint64_t iters = calibrate(data.size());
    ondemand::parser parser;

    auto start = Clock::now();
    for (uint64_t i = 0; i < iters; i++) {
        auto doc = parser.iterate(data);
        std::string_view val;
        auto err = doc.find_field(field).get_string().get(val);
        if (err && i == 0) {
            printf("  %-35s SKIPPED (field '%s' not found)\n", label, field);
            return;
        }
    }
    auto end = Clock::now();
    double secs = std::chrono::duration<double>(end - start).count();
    double mbs = mb_per_sec(data.size() * iters, secs);
    printf("  %-35s %8.1f MB/s  (%llu iters in %.2fs)\n", label, mbs, iters, secs);
}

static void bench_iterate_many_count(const char* label, const padded_string& data) {
    uint64_t iters = calibrate(data.size());
    if (iters > 200) iters = 200; // iterate_many is fast, don't need huge iters

    auto start = Clock::now();
    uint64_t total_docs = 0;
    for (uint64_t i = 0; i < iters; i++) {
        ondemand::parser parser;
        ondemand::document_stream stream = parser.iterate_many(data);
        uint64_t count = 0;
        for (auto doc_result : stream) {
            auto doc = doc_result.value();
            (void)doc;
            count++;
        }
        total_docs += count;
    }
    auto end = Clock::now();
    double secs = std::chrono::duration<double>(end - start).count();
    double mbs = mb_per_sec(data.size() * iters, secs);
    printf("  %-35s %8.1f MB/s  (%llu iters, %llu docs total, %.2fs)\n",
           label, mbs, iters, total_docs, secs);
}

static void bench_iterate_many_extract(const char* label, const padded_string& data,
                                        const char* field) {
    uint64_t iters = calibrate(data.size());
    if (iters > 200) iters = 200;

    auto start = Clock::now();
    uint64_t total_bytes = 0;
    for (uint64_t i = 0; i < iters; i++) {
        ondemand::parser parser;
        ondemand::document_stream stream = parser.iterate_many(data);
        for (auto doc_result : stream) {
            auto doc = doc_result.value();
            std::string_view val;
            auto err = doc.find_field(field).get_string().get(val);
            if (!err) total_bytes += val.size();
        }
    }
    auto end = Clock::now();
    double secs = std::chrono::duration<double>(end - start).count();
    double mbs = mb_per_sec(data.size() * iters, secs);
    printf("  %-35s %8.1f MB/s  (%llu iters, %.2fs)\n", label, mbs, iters, secs);
}

int main(int argc, char** argv) {
    const char* data_dir = "bench/data";
    if (argc > 1) data_dir = argv[1];

    printf("=== C++ simdjson benchmark (no FFI) ===\n\n");

    // Single-file benchmarks
    const char* files[] = {"twitter.json", "citm_catalog.json", "canada.json"};
    for (auto fname : files) {
        std::string path = std::string(data_dir) + "/" + fname;
        padded_string data;
        auto err = padded_string::load(path).get(data);
        if (err) {
            printf("%-40s SKIPPED (file not found)\n", fname);
            continue;
        }
        printf("%s (%zu bytes):\n", fname, data.size());
        bench_ondemand_parse("On-Demand parse", data);

        // Try common field names
        if (std::string(fname) == "twitter.json") {
            bench_ondemand_field("On-Demand find_field(\"search_metadata\")", data, "search_metadata");
        }
        printf("\n");
    }

    // NDJSON benchmarks
    const char* ndjson_files[] = {"100k.ndjson", "1m.ndjson"};
    for (auto fname : ndjson_files) {
        std::string path = std::string(data_dir) + "/" + fname;
        padded_string data;
        auto err = padded_string::load(path).get(data);
        if (err) {
            printf("%-40s SKIPPED (file not found)\n", fname);
            continue;
        }
        printf("%s (%zu bytes):\n", fname, data.size());
        bench_iterate_many_count("iterate_many count", data);
        bench_iterate_many_extract("iterate_many extract(\"name\")", data, "name");
        printf("\n");
    }

    return 0;
}
