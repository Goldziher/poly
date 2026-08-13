//! Tier-1 result-cache key derivation for a hook, plus the worktree-mutation
//! probe that decides whether a passing run may be stored.

use std::path::{Path, PathBuf};

use poly_cache::{CacheKey, InputDigest, Namespace, ResultCache};

use crate::filter::FilePattern;
use crate::git;
use crate::model::{Hook, HookCache, HookCommand};

use super::fixes;

/// The subset of `matched` whose worktree content differs from the index.
///
/// Compared byte-for-byte rather than via `git diff-files`: a stat-based "clean"
/// for a file that really did change would let a passing run be cached under a
/// key derived from content that never passed, and the next hit would replay
/// that pass. Anything unreadable or not a regular file counts as modified — the
/// conservative direction, which only ever suppresses a store.
pub(super) fn modified_matched(root: &Path, matched: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let mut modified = Vec::new();
    for path in matched {
        if !fixes::worktree_matches_staged(root, path)? {
            modified.push(path.clone());
        }
    }
    Ok(modified)
}

/// Derive the [`Namespace::Hook`] cache key for `hook`, or `None` when the hook
/// is not cacheable or its inputs cannot be read.
///
/// The key folds in the hook id, a command-identity `version`, the declared
/// environment (as the `args` table), and a content digest of the relevant
/// input files — so a changed command, env, or input invalidates the entry.
///
/// `git_root` (the real repository) resolves the input *file set* — via
/// `git ls-files` / the matched paths — while `content_root` supplies the
/// *bytes* that are digested. They differ for a workspace hook under isolation,
/// where the list is the tracked tree but the content is the staged snapshot.
pub(super) fn cache_key(git_root: &Path, content_root: &Path, hook: &Hook, matched: &[PathBuf]) -> Option<CacheKey> {
    let digest = match &hook.cache {
        HookCache::Disabled => return None,
        HookCache::MatchedFiles => matched_files_digest(content_root, matched)?,
        HookCache::DeclaredInputs(pattern) => declared_inputs_digest(git_root, content_root, pattern)?,
    };
    let version = hook_version(hook);
    let args = hook_env_table(hook);
    Some(ResultCache::key(Namespace::Hook, &hook.id, &version, &args, &digest))
}

/// Digest the hook's matched files (each as `(relative_path, bytes)`), reading
/// bytes from `content_root`.
///
/// Returns `None` if any matched file cannot be read, which skips caching this
/// hook rather than risk a key derived from partial inputs.
fn matched_files_digest(content_root: &Path, matched: &[PathBuf]) -> Option<InputDigest> {
    read_digest(content_root, matched.iter().cloned())
}

/// Digest every tracked file matching `pattern` — the file set resolved against
/// the whole tree (`git ls-files` under `git_root`), the bytes read from
/// `content_root`.
///
/// Returns `None` if the tree cannot be listed or a matching file is unreadable.
fn declared_inputs_digest(git_root: &Path, content_root: &Path, pattern: &FilePattern) -> Option<InputDigest> {
    let selected = declared_input_files(git_root, pattern).ok()?;
    read_digest(content_root, selected.into_iter())
}

/// The tracked files matching a `DeclaredInputs` pattern (`git ls-files` filtered
/// by the glob). Used both for the digest and the cache-store mutation guard.
pub(super) fn declared_input_files(root: &Path, pattern: &FilePattern) -> anyhow::Result<Vec<PathBuf>> {
    Ok(git::list_files(root)?
        .into_iter()
        .filter(|path| pattern.is_match(path))
        .collect())
}

/// Read the given repo-relative paths and fold them into an [`InputDigest`],
/// sorted by path for a deterministic key. `None` if any read fails.
fn read_digest(content_root: &Path, paths: impl Iterator<Item = PathBuf>) -> Option<InputDigest> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for path in paths {
        let bytes = std::fs::read(content_root.join(&path)).ok()?;
        files.push((path.to_string_lossy().into_owned(), bytes));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Some(ResultCache::file_set_digest(
        files.iter().map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
    ))
}

/// A string capturing the hook's command identity, so a changed command, script
/// target, argument list, or file-passing mode invalidates the cache key.
fn hook_version(hook: &Hook) -> String {
    use std::fmt::Write as _;
    let mut version = String::new();
    match &hook.command {
        HookCommand::Run(line) => version.push_str(line),
        HookCommand::Script { path, runner } => {
            let _ = write!(version, "script\0{runner:?}\0{path}");
        }
    }
    version.push('\0');
    for (index, arg) in hook.args.iter().enumerate() {
        if index > 0 {
            version.push('\0');
        }
        version.push_str(arg);
    }
    let _ = write!(version, "\0pass_filenames={}", hook.pass_filenames);
    version
}

/// The hook's declared environment as a TOML table, so an env change invalidates
/// the cache key. The `BTreeMap` is already ordered, giving a stable table.
fn hook_env_table(hook: &Hook) -> toml::Table {
    hook.env
        .iter()
        .map(|(key, value)| (key.clone(), toml::Value::String(value.clone())))
        .collect()
}
