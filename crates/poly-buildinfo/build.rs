//! Capture the build identity that distinguishes a released `poly` binary from
//! a development build.
//!
//! The identity is deliberately cheap and honest:
//!
//! - **No network access.** Only `git` against the local checkout.
//! - **No dirty-tree probe.** `git describe --dirty` (or a timestamp) would make
//!   two builds of identical sources disagree, breaking reproducible builds. The
//!   identity is a pure function of the committed source tree.
//! - **Graceful fallback.** Outside a git checkout (a source tarball, a vendored
//!   build) the id is empty and the channel resolves to `unknown` — which is the
//!   truth, rather than a fabricated "release".
//!
//! A packager that builds outside git can supply the id explicitly by setting
//! `POLY_BUILD_ID` at build time.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo::rerun-if-env-changed=POLY_BUILD_ID");

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set"));
    register_git_reruns(&manifest_dir);

    let build_id = env_override()
        .or_else(|| git(&manifest_dir, &["describe", "--tags", "--always"]))
        .unwrap_or_default();
    let commit = git(&manifest_dir, &["rev-parse", "--short=12", "HEAD"]).unwrap_or_default();
    let profile = std::env::var("PROFILE").unwrap_or_default();

    println!("cargo::rustc-env=POLY_BUILD_ID={build_id}");
    println!("cargo::rustc-env=POLY_BUILD_COMMIT={commit}");
    println!("cargo::rustc-env=POLY_BUILD_PROFILE={profile}");
}

/// An explicit `POLY_BUILD_ID` from the environment, ignoring blank values.
fn env_override() -> Option<String> {
    std::env::var("POLY_BUILD_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Ask cargo to re-run this script when `HEAD` or the refs move, so a checkout
/// of a different commit never reports the previously-built identity.
fn register_git_reruns(start: &Path) {
    let Some(git_dir) = find_git_dir(start) else {
        return;
    };
    for entry in ["HEAD", "packed-refs", "refs"] {
        let path = git_dir.join(entry);
        if path.exists() {
            println!("cargo::rerun-if-changed={}", path.display());
        }
    }
}

/// The nearest ancestor `.git` **directory**. A `.git` *file* (a worktree or
/// submodule) is skipped: the indirection is not worth parsing here, and the
/// only consequence is that the id refreshes on the next source change instead
/// of on the next checkout.
fn find_git_dir(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        current = dir.parent();
    }
    None
}

/// Run `git` in `dir` and return its trimmed stdout, or `None` when git is
/// absent, the directory is not a checkout, or the command produced nothing.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").current_dir(dir).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}
