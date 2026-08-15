//! Low-level Git primitives for remote sources: command execution, mirror
//! provisioning (with origin-URL verification), and object/revision resolution.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};

/// `GIT_*` variables kept when invoking git against a **foreign** repository.
///
/// Everything needed to *reach* a remote (transport, credentials, explicit
/// `-c`-style config) survives; everything that names a repository, index, or
/// working tree is dropped.
const REMOTE_GIT_ENV_KEEP: &[&str] = &[
    "GIT_ALLOW_PROTOCOL",
    "GIT_ASKPASS",
    "GIT_CONFIG_COUNT",
    "GIT_EXEC_PATH",
    "GIT_HTTP_PROXY_AUTHMETHOD",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_SSL_CAINFO",
    "GIT_SSL_NO_VERIFY",
    "GIT_TERMINAL_PROMPT",
];

/// How many lines of a failed command's stderr are echoed in the error.
const STDERR_LINE_LIMIT: usize = 5;

/// Which of `names` must be removed before invoking git on a foreign repository.
///
/// Pure over the supplied names so the policy is testable without touching the
/// process environment. Non-`GIT_` names are never removed, and the
/// `GIT_CONFIG_KEY_*` / `GIT_CONFIG_VALUE_*` families are kept alongside
/// `GIT_CONFIG_COUNT` because they carry deliberate caller config.
pub fn git_env_to_remove<I, S>(names: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    names
        .into_iter()
        .filter(|name| {
            let name = name.as_ref();
            name.starts_with("GIT_")
                && !name.starts_with("GIT_CONFIG_KEY_")
                && !name.starts_with("GIT_CONFIG_VALUE_")
                && !REMOTE_GIT_ENV_KEEP.contains(&name)
        })
        .map(|name| name.as_ref().to_owned())
        .collect()
}

/// A `git` command scrubbed of inherited repository-scoped `GIT_*` state.
///
/// **Every** git invocation in this module must be built here. These commands
/// operate on *foreign* repositories — a bare mirror and a detached checkout in
/// poly's cache — so consumer-repository state must not follow them in. A git
/// hook is invoked with `GIT_INDEX_FILE` set, and `git commit -a` /
/// `git commit <pathspec>` set it to an **absolute** path; inherited, it makes
/// `git checkout --detach` in a cache checkout reconcile the consumer's index
/// against an unrelated tree, which aborts and prints the consumer's filenames.
///
/// This is deliberately the **opposite** of `poly-hooks`'s `GIT_ENV_TO_REMOVE`
/// (`crates/poly-hooks/src/git.rs`), which intentionally *keeps* `GIT_INDEX_FILE`
/// because its commands act on the **consumer** repository and must see the very
/// index the in-flight commit is building. Same variable, opposing requirements:
/// do not unify the two lists.
pub fn git_command() -> Command {
    let mut command = Command::new("git");
    for name in git_env_to_remove(std::env::vars_os().filter_map(|(key, _)| key.into_string().ok())) {
        command.env_remove(name);
    }
    command
}

/// Run `git -C <directory> <args...>`, failing with stderr on a nonzero exit.
pub fn run_git(directory: &Path, args: &[&str]) -> anyhow::Result<()> {
    run_command(git_command().arg("-C").arg(directory).args(args), "run git")
}

/// Run `git -C <directory> <args...>` and return its trimmed stdout.
pub fn git_output(directory: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = git_command()
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .context("starting git")?;
    if !output.status.success() {
        bail!("git failed: {}", head_lines(&String::from_utf8_lossy(&output.stderr)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run an arbitrary prepared command, failing with its stderr on a nonzero exit.
pub fn run_command(command: &mut Command, operation: &str) -> anyhow::Result<()> {
    let output = command
        .output()
        .with_context(|| format!("{operation}: starting command"))?;
    if !output.status.success() {
        bail!(
            "{operation} failed: {}",
            head_lines(&String::from_utf8_lossy(&output.stderr))
        );
    }
    Ok(())
}

/// The first [`STDERR_LINE_LIMIT`] lines of `text`, noting how many were elided.
///
/// A failing git command can print hundreds of filenames; verbatim, they bury
/// poly's own message and describe a repository the reader did not ask about.
fn head_lines(text: &str) -> String {
    let text = text.trim();
    let total = text.lines().count();
    if total <= STDERR_LINE_LIMIT {
        return text.to_owned();
    }
    let head: Vec<&str> = text.lines().take(STDERR_LINE_LIMIT).collect();
    format!("{} … ({} more lines)", head.join("\n"), total - STDERR_LINE_LIMIT)
}

/// Ensure a bare `--mirror` clone of `url` exists at `mirror`, then verify its
/// stored `remote.origin.url` matches `url`.
///
/// The mirror is materialized atomically (clone into a sibling tempdir, then
/// rename) so a concurrent reader never sees a partial clone. The origin-URL
/// check reads the raw stored value (bypassing the user's Git `insteadOf`
/// rewrites) to block a cache-poisoning source substitution.
pub fn ensure_mirror(mirror: &Path, url: &str) -> anyhow::Result<()> {
    if !mirror.is_dir() {
        let parent = mirror.parent().context("source mirror has no parent")?;
        let temporary = tempfile::Builder::new()
            .prefix("mirror-")
            .tempdir_in(parent)
            .with_context(|| format!("creating temporary source mirror in {}", parent.display()))?;
        let temporary_path = temporary.path().join("repository.git");
        run_command(
            git_command()
                .args(["clone", "--quiet", "--mirror", "--", url])
                .arg(&temporary_path),
            "clone source mirror",
        )?;
        std::fs::rename(&temporary_path, mirror)
            .with_context(|| format!("installing source mirror {}", mirror.display()))?;
    }
    // Read the stored URL without applying the user's Git `insteadOf` rewrites.
    let origin = git_output(mirror, &["config", "--get", "remote.origin.url"])?;
    if origin != url {
        bail!(
            "cached source mirror origin {:?} does not match configured {:?}",
            origin,
            url
        );
    }
    Ok(())
}

/// Ensure `revision` (a full object ID) is present in `mirror`, fetching it from
/// `url` if the mirror does not already contain the object.
pub fn ensure_commit(mirror: &Path, url: &str, revision: &str) -> anyhow::Result<()> {
    if git_object_exists(mirror, revision)? {
        return Ok(());
    }
    run_git(mirror, &["fetch", "--quiet", "origin", revision])
        .with_context(|| format!("fetching locked source commit {revision} from {url}"))?;
    if !git_object_exists(mirror, revision)? {
        bail!("locked source commit {revision} is unavailable from {url}");
    }
    Ok(())
}

/// Whether `revision^{commit}` resolves to an existing object in `repository`.
pub fn git_object_exists(repository: &Path, revision: &str) -> anyhow::Result<bool> {
    Ok(git_command()
        .arg("-C")
        .arg(repository)
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .status()
        .context("checking locked Git revision")?
        .success())
}

/// Validate that `revision` is a full (40- or 64-char) hexadecimal Git object ID.
///
/// A locked revision keys an on-disk checkout directory, so this rejects any
/// value that is not a bare OID — blocking path-traversal (`../…`) and
/// ambiguous-ref attacks before the value reaches the filesystem.
pub fn validate_locked_revision(revision: &str) -> anyhow::Result<()> {
    let valid_length = revision.len() == 40 || revision.len() == 64;
    if !valid_length || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("locked source revision must be a full hexadecimal Git object ID: {revision:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_env_filter_drops_repository_scoped_variables() {
        let removed = git_env_to_remove([
            "GIT_INDEX_FILE",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_SSH_COMMAND",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_CONFIG_COUNT",
            "PATH",
            "HOME",
        ]);
        assert_eq!(removed, vec!["GIT_INDEX_FILE", "GIT_DIR", "GIT_WORK_TREE"]);
    }

    #[test]
    fn head_lines_truncates_and_reports_the_remainder() {
        let long = (1..=9).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        assert_eq!(
            head_lines(&long),
            "line 1\nline 2\nline 3\nline 4\nline 5 … (4 more lines)"
        );
        assert_eq!(head_lines("only\nthese\n"), "only\nthese");
    }
}
