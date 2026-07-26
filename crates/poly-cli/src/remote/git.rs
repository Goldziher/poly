//! Low-level Git primitives for remote sources: command execution, mirror
//! provisioning (with origin-URL verification), and object/revision resolution.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};

/// Run `git -C <directory> <args...>`, failing with stderr on a nonzero exit.
pub fn run_git(directory: &Path, args: &[&str]) -> anyhow::Result<()> {
    run_command(Command::new("git").arg("-C").arg(directory).args(args), "run git")
}

/// Run `git -C <directory> <args...>` and return its trimmed stdout.
pub fn git_output(directory: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .context("starting git")?;
    if !output.status.success() {
        bail!("git failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run an arbitrary prepared command, failing with its stderr on a nonzero exit.
pub fn run_command(command: &mut Command, operation: &str) -> anyhow::Result<()> {
    let output = command
        .output()
        .with_context(|| format!("{operation}: starting command"))?;
    if !output.status.success() {
        bail!("{operation} failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
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
            Command::new("git")
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
    Ok(Command::new("git")
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
