fn main() {
    println!("cargo:rerun-if-changed=../../native");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if os != "linux" || arch != "arm" {
        return;
    }
    cc::Build::new()
        .file("../../native/starfish.c")
        .file("../../native/elog.c")
        .define("_GNU_SOURCE", None)
        .flag("-std=gnu11")
        .flag("-funwind-tables")
        .warnings(false)
        .compile("gfnnative");
    for lib in ["playerAPIs", "pf-1.0", "SDL2"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}
