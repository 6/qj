fn main() {
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .opt_level(3)
        .define("SIMDJSON_EXCEPTIONS", "1")
        .file("src/simdjson/simdjson.cpp")
        .file("src/simdjson/bridge.cpp")
        .include("src/simdjson")
        .compile("simdjson");

    println!("cargo:rerun-if-changed=src/simdjson/bridge.cpp");
    println!("cargo:rerun-if-changed=src/simdjson/simdjson.cpp");
    println!("cargo:rerun-if-changed=src/simdjson/simdjson.h");
}
