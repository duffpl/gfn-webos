//! On the TV, libSDL2 is LG's system fork in the NDK sysroot. On a development host it is whatever
//! `sdl2-config` reports (Homebrew's sdl2-compat on macOS).
fn main() {
    println!("cargo:rerun-if-env-changed=SDL2_CONFIG");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if os == "linux" && arch == "arm" {
        println!("cargo:rustc-link-lib=dylib=SDL2");
        return;
    }
    let config = std::env::var("SDL2_CONFIG").unwrap_or_else(|_| "sdl2-config".into());
    let output = std::process::Command::new(&config)
        .arg("--libs")
        .output()
        .unwrap_or_else(|e| panic!("{config} --libs: {e} (install SDL2, e.g. `brew install sdl2`)"));
    for flag in String::from_utf8_lossy(&output.stdout).split_whitespace() {
        if let Some(dir) = flag.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={dir}");
        } else if let Some(lib) = flag.strip_prefix("-l") {
            println!("cargo:rustc-link-lib=dylib={lib}");
        }
    }
}
