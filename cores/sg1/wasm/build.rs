//! Injects a compile-time git short hash so the WASM build can report a
//! `getBuildId()` identity string (`sg1@<version>+<githash>`). Two
//! netplay peers compare this string in the session handshake and
//! refuse to connect on a mismatch, since a different build can diverge
//! deterministically. Falls back to `unknown` outside a git checkout.

use std::path::Path;
use std::process::Command;

/// The trimmed output of `git <args>`, or `None` when git is missing or
/// this is not a checkout.
fn git(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Asks Cargo to run this script again when `path` changes. Only a file
/// that exists is named: Cargo reruns the script on every build for one
/// that does not.
fn rerun_if_changed(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn main() {
    let git_hash = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_HASH={git_hash}");
    println!("cargo:rerun-if-changed=build.rs");

    // The hash follows the checkout: the script runs again when HEAD moves.
    // A commit moves the branch HEAD names, whose ref is a file of its own
    // or a line of packed-refs; a checkout of another commit rewrites HEAD.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        rerun_if_changed(&Path::new(&git_dir).join("HEAD"));
    }
    if let Some(common_dir) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
        let common_dir = Path::new(&common_dir);
        if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
            rerun_if_changed(&common_dir.join(branch));
        }
        rerun_if_changed(&common_dir.join("packed-refs"));
    }
}
