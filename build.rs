fn main() {
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .opt_level(3)
        .define("SIMDJSON_EXCEPTIONS", "1")
        .warnings(true)
        .flag_if_supported("-Wextra")
        .file("simdjson/simdjson.cpp")
        .file("src/simdjson/bridge.cpp")
        .include("simdjson");

    // simdjson leaves its portable fallback kernel out where a SIMD kernel
    // always runs (arm64), so qj never runs it there. This feature compiles
    // it in anyway (the arm64 kernel stays the default), for the tests and
    // fuzzers that compare every kernel (tests/simdjson_kernels.rs, fuzz/).
    if std::env::var_os("CARGO_FEATURE_SIMDJSON_FALLBACK").is_some() {
        build.define("SIMDJSON_IMPLEMENTATION_FALLBACK", "1");
    }

    // Under cargo-fuzz (`--cfg fuzzing`), instrument simdjson as rustc
    // instruments the Rust code, so that libFuzzer sees which paths through
    // simdjson's kernels an input takes, not only qj's.
    if std::env::var_os("CARGO_CFG_FUZZING").is_some() {
        build.flag_if_supported("-fsanitize-coverage=inline-8bit-counters,pc-table,trace-cmp");
    }

    // Enable sanitizers for C++ when Rust is also compiled with them.
    // Usage: RUSTFLAGS="-Zsanitizer=address" cargo +nightly test
    //   or:  JX_SANITIZE=address cargo +nightly test
    let sanitizer = std::env::var("JX_SANITIZE").ok().or_else(|| {
        let flags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
        flags
            .split('\x1f')
            .find(|f| f.starts_with("-Zsanitizer="))
            .map(|f| f.trim_start_matches("-Zsanitizer=").to_string())
    });
    if let Some(san) = sanitizer {
        for s in san.split(',') {
            build.flag(format!("-fsanitize={s}"));
        }
        build.flag("-fno-omit-frame-pointer");
    }

    build.compile("simdjson");

    // What dyld does before qj's main runs on the stack the user's
    // `RLIMIT_STACK` sizes, and two things made it need more for qj than for
    // jq 1.8.1's release binary, so that qj died at a limit where jq started
    // (see "Small stacks" in docs/COMPATIBILITY.md):
    //
    // - Rust's standard library links libiconv, which qj doesn't use; loading
    //   it costs 144 bytes of stack on macOS 27.
    // - Below a deployment target of 12.0, the linker writes the old
    //   opcode-based binding information rather than chained fixups, and
    //   binding it costs 416 bytes more on macOS 26. Chained fixups load on
    //   macOS 11 and later, which is every arm64 Mac's; x86-64 builds keep the
    //   default, for the macOS releases before 11.
    let target = |key: &str| std::env::var(key).unwrap_or_default();
    if target("CARGO_CFG_TARGET_OS") == "macos" {
        println!("cargo:rustc-link-arg-bins=-Wl,-dead_strip_dylibs");
        if target("CARGO_CFG_TARGET_ARCH") == "aarch64" {
            println!("cargo:rustc-link-arg-bins=-Wl,-fixup_chains");
        }
    }

    // Windows: jq's release binary has no strptime of the C library's, and
    // builds its own (src/jq/platform/strptime.c), so qj does too. And the
    // main thread's stack is what the executable reserves for it (1 MB by
    // default), not `ulimit -s`, so reserve the 256 MB that qj maps for
    // itself elsewhere (src/cli/stack.rs; `STACK_BYTES` in src/cli/run.rs):
    // address space only, committed as it is touched.
    if target("CARGO_CFG_TARGET_OS") == "windows" {
        cc::Build::new()
            .file("src/jq/platform/strptime.c")
            .compile("jq_strptime");
        let stack = 256 << 20;
        if target("CARGO_CFG_TARGET_ENV") == "msvc" {
            println!("cargo:rustc-link-arg-bins=/STACK:{stack}");
        } else {
            println!("cargo:rustc-link-arg-bins=-Wl,--stack,{stack}");
        }
    }

    println!("cargo:rerun-if-changed=src/jq/platform/strptime.c");
    println!("cargo:rerun-if-changed=src/simdjson/bridge.cpp");
    println!("cargo:rerun-if-changed=simdjson/simdjson.cpp");
    println!("cargo:rerun-if-changed=simdjson/simdjson.h");
}
