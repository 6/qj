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

    println!("cargo:rerun-if-changed=src/simdjson/bridge.cpp");
    println!("cargo:rerun-if-changed=simdjson/simdjson.cpp");
    println!("cargo:rerun-if-changed=simdjson/simdjson.h");
}
