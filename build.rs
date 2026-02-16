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

    println!("cargo:rerun-if-changed=src/simdjson/bridge.cpp");
    println!("cargo:rerun-if-changed=simdjson/simdjson.cpp");
    println!("cargo:rerun-if-changed=simdjson/simdjson.h");
}
