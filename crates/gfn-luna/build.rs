fn main() {
    println!("cargo:rerun-if-changed=../../native/luna.c");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if os != "linux" || arch != "arm" {
        return;
    }
    cc::Build::new()
        .file("../../native/luna.c")
        .define("_GNU_SOURCE", None)
        .flag("-std=gnu11")
        .compile("gfnluna");
    println!("cargo:rustc-link-lib=dylib=dl");
    println!("cargo:rustc-link-lib=dylib=pthread");
}
