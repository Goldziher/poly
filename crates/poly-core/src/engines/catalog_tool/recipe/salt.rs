//! What besides a file's own bytes decides a recipe tool's answer, as digests the
//! result cache can key on.
//!
//! - [`config_fingerprint`]: the config files the tool reads (named by flags in
//!   the argv, or found by name walking up from the working directory). Hashed
//!   once per process into the engine's `version()`, the same bounded scope
//!   `native_tool::tool_config` documents.
//! - [`dependency_digest`]: the files of a project root that the verdict depends
//!   on. `helm lint` of one template is a function of the whole chart; a buf
//!   lint of one `.proto` depends on every proto it imports. This one is *per
//!   file* (it varies with the file's project root), so it travels through
//!   `Engine::lint_cache_salt`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;

/// Directories never worth walking for dependencies.
const SKIPPED_DIRECTORIES: &[&str] = &[".git", "node_modules", "target", ".terraform", "__pycache__"];

/// Rewrite the value of each config flag in `tokens` that is a relative path to
/// an absolute one under `base`.
///
/// A tool run from a project root (`helm`, `golangci-lint`) or from a scratch
/// mirror is not in poly's working directory, and `-c checkstyle.xml` written
/// for the repository root would otherwise point at nothing.
pub(super) fn absolutize_config_args(tokens: &[String], flags: &[String], base: &Path) -> Vec<String> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut iter = tokens.iter().peekable();
    while let Some(token) = iter.next() {
        if let Some((flag, value)) = split_assignment(token, flags) {
            out.push(format!("{flag}={}", resolve(base, value)));
        } else if flags.contains(token) {
            out.push(token.clone());
            if let Some(value) = iter.next() {
                out.push(resolve(base, value));
            }
        } else {
            out.push(token.clone());
        }
    }
    out
}

fn split_assignment<'a>(token: &'a str, flags: &[String]) -> Option<(&'a str, &'a str)> {
    let (flag, value) = token.split_once('=')?;
    flags.iter().any(|candidate| candidate == flag).then_some((flag, value))
}

fn resolve(base: &Path, value: &str) -> String {
    let path = Path::new(value);
    if path.is_absolute() {
        value.to_owned()
    } else {
        super::paths::normalize(&base.join(path)).to_string_lossy().into_owned()
    }
}

/// The config files named by `flags` in `tokens`, as paths.
pub(super) fn config_flag_files(tokens: &[String], flags: &[String]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut iter = tokens.iter();
    while let Some(token) = iter.next() {
        if let Some((_, value)) = split_assignment(token, flags) {
            files.push(PathBuf::from(value));
        } else if flags.contains(token)
            && let Some(value) = iter.next()
        {
            files.push(PathBuf::from(value));
        }
    }
    files
}

/// Fingerprint the config the tool will read: the files named by its flags and
/// the files it discovers (`names`) in `cwd` or an ancestor.
pub(super) fn config_fingerprint(cwd: &Path, flagged: &[PathBuf], names: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    for file in flagged {
        hash_file(&mut hasher, file);
    }
    for directory in cwd.ancestors() {
        for name in names {
            let candidate = directory.join(name);
            if candidate.is_file() {
                hash_file(&mut hasher, &candidate);
            }
        }
        if directory.join(".git").exists() {
            break;
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn hash_file(hasher: &mut blake3::Hasher, path: &Path) {
    hasher.update(path.as_os_str().as_encoded_bytes());
    match std::fs::read(path) {
        Ok(bytes) => hasher.update(&bytes),
        Err(_) => hasher.update(b"<unreadable>"),
    };
}

/// Digest of the files under `root` matching `globs` (relative to `root`).
/// Memoized per `(root, globs)` for the process: a run asks once per file of
/// the root, and the tree does not change underneath it.
pub(super) fn dependency_digest(root: &Path, globs: &[String]) -> String {
    type Memo = Mutex<HashMap<(PathBuf, Vec<String>), String>>;
    static MEMO: OnceLock<Memo> = OnceLock::new();
    let memo = MEMO.get_or_init(Default::default);
    let key = (root.to_path_buf(), globs.to_vec());
    if let Some(known) = memo.lock().expect("digest memo poisoned").get(&key) {
        return known.clone();
    }
    let digest = compute_digest(root, globs);
    memo.lock().expect("digest memo poisoned").insert(key, digest.clone());
    digest
}

fn compute_digest(root: &Path, globs: &[String]) -> String {
    let mut builder = GlobSetBuilder::new();
    for pattern in globs {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
        }
    }
    let Ok(set) = builder.build() else {
        return String::new();
    };
    let mut files: Vec<PathBuf> = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .filter_entry(|entry| {
            entry
                .file_name()
                .to_str()
                .is_none_or(|name| !SKIPPED_DIRECTORIES.contains(&name))
        })
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|path| path.strip_prefix(root).is_ok_and(|rel| set.is_match(rel)))
        .collect();
    files.sort();
    let mut hasher = blake3::Hasher::new();
    for file in &files {
        hash_file(&mut hasher, file);
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    #[cfg(unix)]
    fn relative_config_values_become_absolute_under_the_base() {
        let rewritten = absolutize_config_args(
            &strings(&["run", "-c", ".golangci.yml", "--add_tables=./t.json", "./pkg"]),
            &strings(&["-c", "--add_tables"]),
            Path::new("/repo"),
        );
        assert_eq!(
            rewritten,
            strings(&["run", "-c", "/repo/.golangci.yml", "--add_tables=/repo/t.json", "./pkg"])
        );
    }

    #[test]
    #[cfg(unix)]
    fn an_absolute_config_value_is_left_alone() {
        let rewritten = absolutize_config_args(&strings(&["-c", "/etc/x.xml"]), &strings(&["-c"]), Path::new("/repo"));
        assert_eq!(rewritten, strings(&["-c", "/etc/x.xml"]));
    }

    #[test]
    fn config_edits_change_the_fingerprint() {
        let dir = tempfile::tempdir().expect("dir");
        let config = dir.path().join("checkstyle.xml");
        std::fs::write(&config, "<module/>").expect("write");
        let before = config_fingerprint(dir.path(), std::slice::from_ref(&config), &[]);
        std::fs::write(&config, "<module name=\"x\"/>").expect("rewrite");
        assert_ne!(before, config_fingerprint(dir.path(), &[config], &[]));
    }

    #[test]
    fn discovered_config_is_fingerprinted_from_ancestors() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::create_dir(dir.path().join(".git")).expect("git");
        std::fs::create_dir(dir.path().join("sub")).expect("sub");
        let names = strings(&[".hadolint.yaml"]);
        let before = config_fingerprint(&dir.path().join("sub"), &[], &names);
        std::fs::write(dir.path().join(".hadolint.yaml"), "ignored: [DL3008]\n").expect("write");
        assert_ne!(before, config_fingerprint(&dir.path().join("sub"), &[], &names));
    }

    #[test]
    fn the_dependency_digest_follows_matching_files_only() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("a.proto"), "message A {}").expect("a");
        std::fs::write(dir.path().join("notes.txt"), "one").expect("notes");
        let globs = strings(&["**/*.proto"]);
        let first = compute_digest(dir.path(), &globs);
        std::fs::write(dir.path().join("notes.txt"), "two").expect("notes");
        assert_eq!(
            first,
            compute_digest(dir.path(), &globs),
            "unrelated files are not dependencies"
        );
        std::fs::write(dir.path().join("a.proto"), "message B {}").expect("a");
        assert_ne!(
            first,
            compute_digest(dir.path(), &globs),
            "a dependency edit changes the digest"
        );
    }
}
