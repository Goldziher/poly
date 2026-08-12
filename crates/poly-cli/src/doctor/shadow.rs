//! Which `poly` a bare `poly` invocation resolves to, and whether that is the
//! executable currently running.
//!
//! Two installs of `poly` on one `PATH` is the single defect behind most
//! "poly reported X but did Y" confusion: `brew upgrade poly` succeeds while
//! `poly --version` never moves, because a cargo-installed `~/.cargo/bin/poly`
//! comes first on a default developer `PATH`.
//!
//! Everything in this module is **subprocess-free**. It compares file identity
//! (canonical path) only, so [`warn_if_conflicting`] costs a handful of `stat`
//! calls and can run on every invocation. Asking each binary what version it is
//! costs a process spawn and lives in [`super::probe`], on the `poly doctor`
//! path only.

use std::path::{Path, PathBuf};

/// File name of the poly executable on this platform.
pub const EXECUTABLE_NAME: &str = if cfg!(windows) { "poly.exe" } else { "poly" };

/// Set to any non-empty value to silence the PATH-conflict warning.
pub const SUPPRESS_ENV: &str = "POLY_NO_SHADOW_WARN";

/// One `poly` executable found on `PATH`, recorded in `PATH` order.
#[derive(Debug, Clone)]
pub struct PathEntry {
    /// The `PATH` directory it was found in, exactly as `PATH` spelled it.
    pub directory: PathBuf,
    /// `directory` joined with the executable name.
    pub path: PathBuf,
    /// The path with symlinks resolved — the file's identity, used for
    /// comparison so a symlink farm is not mistaken for a second install.
    pub canonical: PathBuf,
}

impl PathEntry {
    /// Whether this entry lives in a cargo bin directory (`~/.cargo/bin`, or
    /// `$CARGO_HOME/bin`).
    ///
    /// Worth singling out because `cargo uninstall poly` does **not** remove it:
    /// cargo resolves the *package* name, finds no `poly` package installed, and
    /// fails with "a package with a similar name exists: polyfmt" — leaving the
    /// shadowing binary in place. The remedy is `rm`.
    pub fn is_cargo_bin(&self) -> bool {
        let Some(parent) = self.canonical.parent() else {
            return false;
        };
        cargo_bin_dirs().iter().any(|dir| parent == dir)
    }

    /// The command that actually removes this executable.
    pub fn removal_hint(&self) -> String {
        if self.is_cargo_bin() {
            format!(
                "rm {} (`cargo uninstall poly` does not remove it — it resolves a package name, not this file)",
                self.path.display()
            )
        } else {
            format!(
                "rm {} (or uninstall it with the package manager that placed it there)",
                self.path.display()
            )
        }
    }
}

/// Candidate cargo bin directories: `$CARGO_HOME/bin` and `~/.cargo/bin`.
fn cargo_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME") {
        dirs.push(PathBuf::from(cargo_home).join("bin"));
    }
    if let Some(home) = home_dir() {
        dirs.push(home.join(".cargo").join("bin"));
    }
    dirs.iter().filter_map(|dir| dir.canonicalize().ok()).collect()
}

/// The user's home directory, without pulling in a dependency for one lookup.
fn home_dir() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// The running executable and every `poly` on `PATH`, in `PATH` order.
#[derive(Debug, Clone)]
pub struct Resolution {
    /// The canonical path of the running executable, when it can be determined.
    pub running: Option<PathBuf>,
    /// Every `poly` found on `PATH`, in order, deduplicated by file identity.
    pub entries: Vec<PathEntry>,
    /// Index into [`Resolution::entries`] of the running executable, when the
    /// running executable is itself reachable through `PATH`.
    pub running_index: Option<usize>,
}

impl Resolution {
    /// Scan `PATH` and locate the running executable within it.
    pub fn detect() -> Self {
        let running = std::env::current_exe()
            .ok()
            .map(|path| path.canonicalize().unwrap_or(path));
        let entries = scan_path();
        let running_index = running
            .as_ref()
            .and_then(|running| entries.iter().position(|entry| &entry.canonical == running));
        Resolution {
            running,
            entries,
            running_index,
        }
    }

    /// Whether the running executable was reachable through `PATH` at all.
    ///
    /// `false` means it was invoked by an explicit path (`./target/debug/poly`,
    /// a CI artifact, a `cargo run`), where a different `poly` on `PATH` is
    /// expected and not worth a warning.
    pub fn resolved_from_path(&self) -> bool {
        self.running_index.is_some()
    }

    /// Entries **ahead** of the running executable on `PATH`: a bare `poly`
    /// would run one of these, not the binary producing this output.
    pub fn shadowing(&self) -> &[PathEntry] {
        match self.running_index {
            Some(index) => &self.entries[..index],
            None => &[],
        }
    }

    /// Entries **behind** the running executable on `PATH`: installs this binary
    /// hides, so upgrading any of them changes nothing a bare `poly` observes.
    pub fn shadowed(&self) -> &[PathEntry] {
        match self.running_index {
            Some(index) => &self.entries[index + 1..],
            None => &[],
        }
    }

    /// Whether `PATH` holds a second, different `poly` alongside the running one.
    pub fn has_conflict(&self) -> bool {
        self.resolved_from_path() && self.entries.len() > 1
    }

    /// What a bare `poly` resolves to, whichever binary is running.
    pub fn first(&self) -> Option<&PathEntry> {
        self.entries.first()
    }
}

/// Every `poly` on `PATH`, in order, skipping repeated directories and repeated
/// files (so a `PATH` listing the same install twice reports one entry).
pub fn scan_path() -> Vec<PathEntry> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut entries: Vec<PathEntry> = Vec::new();
    for directory in std::env::split_paths(&path) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        let candidate = directory.join(EXECUTABLE_NAME);
        if !is_executable_file(&candidate) {
            continue;
        }
        let canonical = candidate.canonicalize().unwrap_or_else(|_| candidate.clone());
        if entries.iter().any(|entry| entry.canonical == canonical) {
            continue;
        }
        entries.push(PathEntry {
            directory,
            path: candidate,
            canonical,
        });
    }
    entries
}

/// Whether `path` is a regular file with an execute bit set.
#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Whether `path` is a regular file. Windows has no execute bit; presence in a
/// `PATH` directory under the executable name is the whole signal.
#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|metadata| metadata.is_file())
}

/// Warn on stderr when `PATH` holds a `poly` other than the running one.
///
/// Fires only when the running executable is itself reachable through `PATH`,
/// which is the case where the user typed `poly` and could reasonably believe
/// the answer came from the install they last upgraded. Running an explicit path
/// (`./target/debug/poly`) never warns.
///
/// A correctly-installed poly finds exactly one entry and prints nothing, so the
/// warning is defect-conditional rather than routine noise. Set
/// [`SUPPRESS_ENV`] to silence it regardless.
pub fn warn_if_conflicting() {
    if std::env::var_os(SUPPRESS_ENV).is_some_and(|value| !value.is_empty()) {
        return;
    }
    let resolution = Resolution::detect();
    if !resolution.has_conflict() {
        return;
    }
    let running = resolution
        .running
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());

    if let Some(ahead) = resolution.shadowing().first() {
        eprintln!("warning: a different `poly` comes earlier on PATH than the one that produced this output");
        eprintln!("  running:      {running}");
        eprintln!("  `poly` runs:  {}", ahead.path.display());
    } else if let Some(behind) = resolution.shadowed().first() {
        eprintln!("warning: this `poly` hides another install later on PATH");
        eprintln!("  running:      {running}");
        eprintln!("  also on PATH: {}", behind.path.display());
        eprintln!("  upgrading the hidden install will not change what `poly` runs");
    }
    eprintln!("  run `poly doctor` for versions and the remedy (set {SUPPRESS_ENV}=1 to silence)");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> PathEntry {
        let path = PathBuf::from(path);
        PathEntry {
            directory: path.parent().unwrap().to_path_buf(),
            canonical: path.clone(),
            path,
        }
    }

    fn resolution(paths: &[&str], running_index: Option<usize>) -> Resolution {
        let entries: Vec<PathEntry> = paths.iter().map(|p| entry(p)).collect();
        Resolution {
            running: running_index.map(|index| entries[index].canonical.clone()),
            entries,
            running_index,
        }
    }

    #[test]
    fn single_install_has_no_conflict() {
        let resolution = resolution(&["/opt/homebrew/bin/poly"], Some(0));
        assert!(!resolution.has_conflict(), "one poly on PATH is not a conflict");
        assert!(resolution.shadowing().is_empty());
        assert!(resolution.shadowed().is_empty());
    }

    #[test]
    fn running_first_reports_the_installs_it_hides() {
        // The real incident: `~/.cargo/bin/poly` wins, so `brew upgrade` moves
        // a binary that nothing ever runs.
        let resolution = resolution(&["/home/u/.cargo/bin/poly", "/opt/homebrew/bin/poly"], Some(0));
        assert!(resolution.has_conflict());
        assert!(resolution.shadowing().is_empty(), "nothing is ahead of the running one");
        assert_eq!(resolution.shadowed().len(), 1, "one hidden install");
        assert_eq!(resolution.shadowed()[0].path, PathBuf::from("/opt/homebrew/bin/poly"));
    }

    #[test]
    fn running_second_reports_the_install_ahead_of_it() {
        let resolution = resolution(&["/home/u/.cargo/bin/poly", "/opt/homebrew/bin/poly"], Some(1));
        assert!(resolution.has_conflict());
        assert_eq!(resolution.shadowing().len(), 1, "one install ahead on PATH");
        assert_eq!(resolution.shadowing()[0].path, PathBuf::from("/home/u/.cargo/bin/poly"));
        assert!(resolution.shadowed().is_empty());
    }

    #[test]
    fn explicit_invocation_is_never_a_conflict() {
        // `./target/debug/poly` is not on PATH: a developer running a local
        // build must not be nagged about their installed poly.
        let resolution = resolution(&["/home/u/.cargo/bin/poly", "/opt/homebrew/bin/poly"], None);
        assert!(!resolution.resolved_from_path());
        assert!(!resolution.has_conflict(), "explicit-path invocation is not a conflict");
        assert!(resolution.shadowing().is_empty());
        assert!(resolution.shadowed().is_empty());
    }

    #[test]
    fn removal_hint_for_a_non_cargo_path_suggests_rm() {
        let hint = entry("/opt/homebrew/bin/poly").removal_hint();
        assert!(hint.starts_with("rm /opt/homebrew/bin/poly"), "{hint}");
    }

    #[test]
    fn a_non_executable_file_is_not_a_path_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(EXECUTABLE_NAME);
        std::fs::write(&path, b"not a binary").unwrap();
        assert!(
            !is_executable_file(&path),
            "a mode-644 file named `poly` is not an install"
        );
        assert!(!is_executable_file(dir.path()), "a directory is not an install");
        assert!(
            !is_executable_file(&dir.path().join("absent")),
            "a missing file is not an install"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_executable_file_is_a_path_entry() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(EXECUTABLE_NAME);
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable_file(&path), "a mode-755 `poly` is an install");
    }
}
