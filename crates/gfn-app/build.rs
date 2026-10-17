//! Bakes in the app's version from pkg/appinfo.json (CI writes the tag's version there, and webOS
//! reads it), the commit and its date, and the GitHub repository releases are checked against.

use std::path::Path;
use std::process::Command;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let appinfo = root.join("pkg/appinfo.json");
    println!("cargo:rerun-if-changed={}", appinfo.display());
    let text = std::fs::read_to_string(&appinfo).unwrap_or_else(|e| panic!("{}: {e}", appinfo.display()));
    let info: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", appinfo.display()));
    let version = info["version"].as_str().unwrap_or_else(|| panic!("{}: no version", appinfo.display()));
    println!("cargo:rustc-env=GFN_VERSION={version}");

    // A build from a source archive has no git; it shows the version alone.
    let git = |args: &[&str]| {
        let output = Command::new("git").arg("-C").arg(&root).args(args).output().ok()?;
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    println!("cargo:rustc-env=GFN_COMMIT={}", git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_default());
    println!("cargo:rustc-env=GFN_COMMIT_DATE={}", git(&["log", "-1", "--format=%cs"]).unwrap_or_default());
    // A commit moves HEAD or the branch it names. Only existing files: cargo reruns every build
    // for a missing one.
    let mut watched = vec![git(&["rev-parse", "--git-path", "HEAD"])];
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watched.push(git(&["rev-parse", "--git-path", &branch]));
        watched.push(git(&["rev-parse", "--git-path", "packed-refs"]));
    }
    for path in watched.into_iter().flatten().map(|path| root.join(path)).filter(|path| path.is_file()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let repository = std::env::var("CARGO_PKG_REPOSITORY").unwrap_or_default();
    let repo = repository.trim_end_matches('/').strip_prefix("https://github.com/").unwrap_or_else(|| {
        panic!("repository {repository:?}: releases are looked up on GitHub, so it must be https://github.com/OWNER/REPO")
    });
    println!("cargo:rustc-env=GFN_REPO={repo}");
}
