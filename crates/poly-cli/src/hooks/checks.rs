//! Pure-Rust file-safety checks — the in-process replacement for the
//! pre-commit-hooks file-safety block.
//!
//! These back the `file_safety` builtin group (`[hooks.builtin.file_safety]`).
//! Lowering ([`crate::hooks::lower`]) turns the enabled member checks into a
//! single hidden `poly hooks check …` invocation whose matched files are
//! appended by the runner; [`run_file_safety_checks`] then runs each requested
//! check over that file set, prints one line per problem, and exits non-zero
//! when anything fails.
//!
//! Each check is a free function over `(root, files)` so it can be unit-tested
//! against a temporary directory without going through the CLI. Files are read
//! relative to `root`; a path that cannot be read (e.g. a staged deletion) is
//! skipped rather than reported, so the checks never fail on a vanished file.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::LazyLock;

use aho_corasick::AhoCorasick;
use anyhow::{Context, Result};
use clap::Args;
use poly_config::DEFAULT_MAX_ADDED_FILE_KB;

/// Number of bytes per kibibyte, for the large-file size ceiling.
const BYTES_PER_KIB: u64 = 1024;

/// Upper bound on the file size the content-scanning checks
/// ([`check_merge_conflict`], [`check_private_key`]) will read into memory. A
/// hand-edited conflict or a committed private key is never multi-megabyte, so
/// skipping larger files preserves the checks' intent while bounding memory
/// (a staged binary blob can no longer trigger an OOM).
const MAX_CONTENT_SCAN_BYTES: u64 = 10 * 1024 * 1024;

/// Private-key headers rejected by [`check_private_key`]. Mirrors the
/// pre-commit-hooks `detect-private-key` blocklist.
const PRIVATE_KEY_MARKERS: &[&str] = &[
    "BEGIN RSA PRIVATE KEY",
    "BEGIN DSA PRIVATE KEY",
    "BEGIN EC PRIVATE KEY",
    "BEGIN OPENSSH PRIVATE KEY",
    "BEGIN PRIVATE KEY",
    "BEGIN ENCRYPTED PRIVATE KEY",
    "BEGIN PGP PRIVATE KEY BLOCK",
    "BEGIN OpenVPN Static key V1",
    "PuTTY-User-Key-File-",
    "SSH PRIVATE KEY",
];

/// Git merge-conflict marker prefixes. A line is a marker when it equals one of
/// these exactly or is the prefix followed by a space (`<<<<<<< branch`), which
/// avoids flagging longer rules like `========` (eight `=`) used as headings.
const CONFLICT_MARKERS: &[&str] = &["<<<<<<<", "=======", ">>>>>>>", "|||||||"];

/// Aho-Corasick automaton over [`PRIVATE_KEY_MARKERS`], built once. A single
/// SIMD-accelerated pass over the raw file bytes replaces the previous
/// ten-marker `str::contains` loop, and `find` reports which marker matched.
///
/// Construction is infallible for these compile-time-constant ASCII patterns;
/// `expect` documents that invariant.
static PRIVATE_KEY_AUTOMATON: LazyLock<AhoCorasick> =
    LazyLock::new(|| AhoCorasick::new(PRIVATE_KEY_MARKERS).expect("private-key markers compile"));

/// Aho-Corasick automaton over [`CONFLICT_MARKERS`], built once. Used only as a
/// cheap skip-fast-path: if none of the marker prefixes appears anywhere in a
/// file, it cannot contain a marker line, so the precise per-line scan is
/// skipped without allocating. Files that match still go through the exact
/// line-level check, so behavior is unchanged for real conflict files.
static CONFLICT_MARKER_AUTOMATON: LazyLock<AhoCorasick> =
    LazyLock::new(|| AhoCorasick::new(CONFLICT_MARKERS).expect("conflict markers compile"));

/// Read `path` for a content-scanning check, returning `None` (skip) when it
/// cannot be stat'd, is not a regular file, or exceeds [`MAX_CONTENT_SCAN_BYTES`].
fn read_for_scan(path: &Path) -> Option<Vec<u8>> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CONTENT_SCAN_BYTES {
        return None;
    }
    std::fs::read(path).ok()
}

/// `poly hooks check` — the hidden subcommand the file-safety builtin lowers to.
///
/// Each `--<check>` flag turns on one member check; the trailing positional
/// arguments are the matched files appended by the hook runner.
#[derive(Args, Debug, Default)]
pub struct CheckArgs {
    /// Reject files containing git merge-conflict markers.
    #[arg(long)]
    pub merge_conflict: bool,
    /// Reject files larger than `--max-added-kb`.
    #[arg(long)]
    pub added_large_files: bool,
    /// Size ceiling, in kibibytes, for the large-file check.
    #[arg(long, default_value_t = DEFAULT_MAX_ADDED_FILE_KB)]
    pub max_added_kb: u64,
    /// Reject files containing a private-key header.
    #[arg(long)]
    pub private_key: bool,
    /// Reject paths that collide case-insensitively.
    #[arg(long)]
    pub case_conflict: bool,
    /// Require executable files to start with a `#!` shebang.
    #[arg(long)]
    pub executables_have_shebangs: bool,
    /// Require files starting with `#!` to be executable.
    #[arg(long)]
    pub shebang_scripts_are_executable: bool,
    /// The matched files to check (appended by the hook runner).
    pub files: Vec<PathBuf>,
}

/// A single file-safety problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The check that produced the problem (e.g. `"check-merge-conflict"`).
    pub check: &'static str,
    /// The offending path, relative to the repository root.
    pub path: PathBuf,
    /// A human-readable description of the problem.
    pub message: String,
}

impl Violation {
    fn new(check: &'static str, path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            check,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {} [{}]",
            self.path.display(),
            self.message,
            self.check
        )
    }
}

/// Run every requested file-safety check over the given files and map the
/// outcome to a process exit code.
///
/// Problems are printed one per line to stderr, followed by a count. Returns
/// [`ExitCode::SUCCESS`] when no problem is found.
///
/// # Errors
///
/// Returns `Err` only if the current working directory cannot be resolved; the
/// individual checks never error (an unreadable file is skipped).
pub fn run_file_safety_checks(args: &CheckArgs) -> Result<ExitCode> {
    let root = std::env::current_dir().context("failed to resolve the working directory")?;
    let violations = collect_violations(&root, args);

    if violations.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    for violation in &violations {
        eprintln!("{violation}");
    }
    eprintln!("file-safety: {} problem(s) found", violations.len());
    Ok(ExitCode::FAILURE)
}

/// Collect the problems from every check requested in `args`.
fn collect_violations(root: &Path, args: &CheckArgs) -> Vec<Violation> {
    let mut violations = Vec::new();
    if args.merge_conflict {
        violations.extend(check_merge_conflict(root, &args.files));
    }
    if args.added_large_files {
        violations.extend(check_added_large_files(
            root,
            &args.files,
            args.max_added_kb,
        ));
    }
    if args.private_key {
        violations.extend(check_private_key(root, &args.files));
    }
    if args.case_conflict {
        violations.extend(check_case_conflict(&args.files));
    }
    if args.executables_have_shebangs {
        violations.extend(check_executables_have_shebangs(root, &args.files));
    }
    if args.shebang_scripts_are_executable {
        violations.extend(check_shebang_scripts_are_executable(root, &args.files));
    }
    violations
}

// ── individual checks ─────────────────────────────────────────────────────────

/// Whether `line` is a git merge-conflict marker line.
fn is_conflict_marker(line: &str) -> bool {
    CONFLICT_MARKERS.iter().any(|marker| {
        line == *marker
            || line
                .strip_prefix(marker)
                .is_some_and(|rest| rest.starts_with(' '))
    })
}

/// Reject files containing git merge-conflict markers, reporting each marker
/// line.
fn check_merge_conflict(root: &Path, files: &[PathBuf]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for path in files {
        let Some(bytes) = read_for_scan(&root.join(path)) else {
            continue;
        };
        // Fast skip: no marker prefix anywhere means no marker line is possible.
        if !CONFLICT_MARKER_AUTOMATON.is_match(&bytes) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for (index, line) in text.lines().enumerate() {
            if is_conflict_marker(line) {
                violations.push(Violation::new(
                    "check-merge-conflict",
                    path.clone(),
                    format!("merge-conflict marker on line {}", index + 1),
                ));
            }
        }
    }
    violations
}

/// Reject files whose size exceeds `max_kb` kibibytes.
fn check_added_large_files(root: &Path, files: &[PathBuf], max_kb: u64) -> Vec<Violation> {
    let limit = max_kb.saturating_mul(BYTES_PER_KIB);
    let mut violations = Vec::new();
    for path in files {
        let Ok(metadata) = std::fs::metadata(root.join(path)) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let size = metadata.len();
        if size > limit {
            violations.push(Violation::new(
                "check-added-large-files",
                path.clone(),
                format!(
                    "file is {} KiB, over the {max_kb} KiB limit",
                    size / BYTES_PER_KIB
                ),
            ));
        }
    }
    violations
}

/// Reject files containing any known private-key header.
fn check_private_key(root: &Path, files: &[PathBuf]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for path in files {
        let Some(bytes) = read_for_scan(&root.join(path)) else {
            continue;
        };
        // Single SIMD pass over the raw bytes; the markers are ASCII so no
        // UTF-8 conversion is needed. `pattern()` indexes back into the marker
        // list to name which header matched.
        if let Some(found) = PRIVATE_KEY_AUTOMATON.find(&bytes) {
            let marker = PRIVATE_KEY_MARKERS[found.pattern().as_usize()];
            violations.push(Violation::new(
                "detect-private-key",
                path.clone(),
                format!("contains a private-key header (`{marker}`)"),
            ));
        }
    }
    violations
}

/// Reject paths that collide when compared case-insensitively.
///
/// Reports one problem per colliding group, naming every member, so a
/// case-insensitive filesystem cannot end up with two of them checked out.
fn check_case_conflict(files: &[PathBuf]) -> Vec<Violation> {
    use std::collections::{BTreeMap, HashSet};

    let mut groups: BTreeMap<String, Vec<&PathBuf>> = BTreeMap::new();
    for path in files {
        let key = path.to_string_lossy().to_lowercase();
        groups.entry(key).or_default().push(path);
    }

    let mut violations = Vec::new();
    for members in groups.values() {
        // A real collision needs two paths that differ only by case; identical
        // paths repeated in the input are not a conflict. Track membership in a
        // set (O(1) lookup) while preserving first-seen order for the report.
        let mut seen: HashSet<&PathBuf> = HashSet::new();
        let mut distinct: Vec<&PathBuf> = Vec::new();
        for member in members {
            if seen.insert(member) {
                distinct.push(member);
            }
        }
        if distinct.len() < 2 {
            continue;
        }
        let names = distinct
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(", ");
        violations.push(Violation::new(
            "check-case-conflict",
            distinct[0].clone(),
            format!("paths collide case-insensitively: {names}"),
        ));
    }
    violations
}

/// Whether `path` begins with a `#!` shebang, reading only the first two bytes.
///
/// Returns `false` when the file is unreadable or shorter than the prefix; never
/// reads the whole file (a large committed binary is not buffered into memory).
fn starts_with_shebang(path: &Path) -> bool {
    use std::io::Read as _;

    let mut prefix = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut prefix))
        .is_ok_and(|()| prefix == *b"#!")
}

/// Require executable files to begin with a `#!` shebang.
///
/// On non-Unix targets, file mode bits are unavailable, so this check is a
/// no-op.
fn check_executables_have_shebangs(root: &Path, files: &[PathBuf]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for path in files {
        let absolute = root.join(path);
        if !is_executable(&absolute) {
            continue;
        }
        if !starts_with_shebang(&absolute) {
            violations.push(Violation::new(
                "check-executables-have-shebangs",
                path.clone(),
                "executable file does not start with a `#!` shebang",
            ));
        }
    }
    violations
}

/// Require files that begin with a `#!` shebang to be executable.
///
/// On non-Unix targets, file mode bits are unavailable, so this check is a
/// no-op: `is_executable` is unconditionally `false` there, so without this
/// guard every shebang script would be falsely flagged.
fn check_shebang_scripts_are_executable(root: &Path, files: &[PathBuf]) -> Vec<Violation> {
    #[cfg(unix)]
    {
        check_shebang_scripts_are_executable_unix(root, files)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, files);
        Vec::new()
    }
}

/// Unix implementation of [`check_shebang_scripts_are_executable`].
#[cfg(unix)]
fn check_shebang_scripts_are_executable_unix(root: &Path, files: &[PathBuf]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for path in files {
        let absolute = root.join(path);
        if !absolute.is_file() || !starts_with_shebang(&absolute) {
            continue;
        }
        if !is_executable(&absolute) {
            violations.push(Violation::new(
                "check-shebang-scripts-are-executable",
                path.clone(),
                "file starts with a `#!` shebang but is not executable",
            ));
        }
    }
    violations
}

/// Whether `path` is a regular file with any execute bit set.
///
/// Always `false` on non-Unix targets, where mode bits are not represented.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, name: &str, contents: &[u8]) -> PathBuf {
        let path = PathBuf::from(name);
        fs::write(root.join(&path), contents).unwrap();
        path
    }

    #[test]
    fn merge_conflict_flags_each_marker_line() {
        let dir = tempfile::tempdir().unwrap();
        let clean = write(dir.path(), "clean.txt", b"all good\n=========== heading\n");
        let bad = write(
            dir.path(),
            "bad.txt",
            b"line one\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> branch\n",
        );

        let violations = check_merge_conflict(dir.path(), &[clean, bad.clone()]);
        assert_eq!(violations.len(), 3, "{violations:?}");
        assert!(violations.iter().all(|v| v.path == bad));
        assert_eq!(violations[0].check, "check-merge-conflict");
        assert!(violations[0].message.contains("line 2"));
    }

    #[test]
    fn merge_conflict_ignores_long_equals_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "doc.md", b"Title\n========\nbody\n");
        assert!(check_merge_conflict(dir.path(), &[path]).is_empty());
    }

    #[test]
    fn large_file_flags_only_over_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let big = write(dir.path(), "big.bin", &vec![0u8; 3 * 1024]);
        let small = write(dir.path(), "small.bin", &vec![0u8; 512]);

        let violations = check_added_large_files(dir.path(), &[big.clone(), small], 1);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, big);
        assert_eq!(violations[0].check, "check-added-large-files");
    }

    #[test]
    fn private_key_flags_known_headers() {
        let dir = tempfile::tempdir().unwrap();
        let key = write(
            dir.path(),
            "id_rsa",
            b"-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n",
        );
        let clean = write(dir.path(), "notes.txt", b"nothing secret here\n");

        let violations = check_private_key(dir.path(), &[key.clone(), clean]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, key);
        assert!(violations[0].message.contains("OPENSSH PRIVATE KEY"));
    }

    #[test]
    fn content_checks_skip_files_over_the_size_cap() {
        let dir = tempfile::tempdir().unwrap();
        // A file larger than the scan cap is skipped even if it contains a
        // marker, so a huge staged blob can never be read into memory.
        let mut blob = vec![b'x'; (MAX_CONTENT_SCAN_BYTES as usize) + 1];
        blob.extend_from_slice(b"\n-----BEGIN OPENSSH PRIVATE KEY-----\n");
        blob.extend_from_slice(b"<<<<<<< HEAD\n");
        let big = write(dir.path(), "huge.bin", &blob);

        assert!(check_private_key(dir.path(), std::slice::from_ref(&big)).is_empty());
        assert!(check_merge_conflict(dir.path(), &[big]).is_empty());
    }

    #[test]
    fn case_conflict_flags_paths_differing_only_by_case() {
        let lower = PathBuf::from("src/File.rs");
        let upper = PathBuf::from("src/file.rs");
        let other = PathBuf::from("src/main.rs");

        let violations = check_case_conflict(&[lower, upper, other]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].check, "check-case-conflict");
        assert!(violations[0].message.contains("File.rs"));
        assert!(violations[0].message.contains("file.rs"));
    }

    #[test]
    fn case_conflict_ignores_repeated_identical_paths() {
        let path = PathBuf::from("a.txt");
        assert!(check_case_conflict(&[path.clone(), path]).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn executable_without_shebang_is_flagged() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "tool", b"not a script\n");
        fs::set_permissions(dir.path().join(&path), fs::Permissions::from_mode(0o755)).unwrap();

        let violations = check_executables_have_shebangs(dir.path(), std::slice::from_ref(&path));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, path);
        assert_eq!(violations[0].check, "check-executables-have-shebangs");
    }

    #[test]
    #[cfg(unix)]
    fn executable_with_shebang_is_clean() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "tool.sh", b"#!/bin/sh\necho hi\n");
        fs::set_permissions(dir.path().join(&path), fs::Permissions::from_mode(0o755)).unwrap();

        assert!(check_executables_have_shebangs(dir.path(), &[path]).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn shebang_script_without_exec_bit_is_flagged() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "script.sh", b"#!/bin/sh\necho hi\n");
        fs::set_permissions(dir.path().join(&path), fs::Permissions::from_mode(0o644)).unwrap();

        let violations =
            check_shebang_scripts_are_executable(dir.path(), std::slice::from_ref(&path));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, path);
        assert_eq!(violations[0].check, "check-shebang-scripts-are-executable");
    }

    #[test]
    fn collect_violations_runs_only_requested_checks() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "bad.txt", b"<<<<<<< HEAD\n");
        // Only the large-file check is enabled, so the merge marker is ignored.
        let args = CheckArgs {
            added_large_files: true,
            max_added_kb: 1,
            files: vec![path],
            ..CheckArgs::default()
        };
        assert!(collect_violations(dir.path(), &args).is_empty());
    }

    #[test]
    fn unreadable_files_are_skipped_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        let missing = PathBuf::from("does-not-exist.txt");
        assert!(check_merge_conflict(dir.path(), std::slice::from_ref(&missing)).is_empty());
        assert!(check_added_large_files(dir.path(), std::slice::from_ref(&missing), 1).is_empty());
        assert!(check_private_key(dir.path(), &[missing]).is_empty());
    }
}
