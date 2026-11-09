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

/// Ensure a bare repository for `url` exists at `mirror`, then verify its stored
/// `remote.origin.url` matches `url`.
///
/// The repository starts empty: commits are fetched into it one at a time, and
/// shallowly, by [`ensure_commit`]. Cloning the whole history up front made a
/// cold `poly lock` download every object the source ever had (issue #157). A
/// mirror left by an older poly (a full `--mirror` clone) is reused as is.
///
/// The repository is materialized atomically (initialized in a sibling tempdir,
/// then renamed) so a concurrent reader never sees a half-configured one. The
/// origin-URL check reads the raw stored value (bypassing the user's Git
/// `insteadOf` rewrites) to block a cache-poisoning source substitution.
pub fn ensure_mirror(mirror: &Path, url: &str) -> anyhow::Result<()> {
    if !mirror.is_dir() {
        let parent = mirror.parent().context("source mirror has no parent")?;
        let temporary = tempfile::Builder::new()
            .prefix("mirror-")
            .tempdir_in(parent)
            .with_context(|| format!("creating temporary source mirror in {}", parent.display()))?;
        let temporary_path = temporary.path().join("repository.git");
        run_command(
            git_command().args(["init", "--quiet", "--bare"]).arg(&temporary_path),
            "initialize source mirror",
        )?;
        run_git(&temporary_path, &["remote", "add", "origin", url]).context("configuring source mirror origin")?;
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
///
/// Only that one commit is fetched, at depth 1, and kept reachable under the
/// branch `refs/heads/poly-pinned/<oid>` — a branch, because the checkout is a
/// `git clone` of the mirror and a clone copies only branches and tags. A server that refuses a request for an object it
/// did not advertise (protocol v0 without `uploadpack.allowAnySHA1InWant`)
/// falls back to fetching every branch and tag.
pub fn ensure_commit(mirror: &Path, url: &str, revision: &str) -> anyhow::Result<()> {
    if git_object_exists(mirror, revision)? {
        return Ok(());
    }
    poly_config::network::require_online(&format!("fetch locked source commit {revision} from {url}"))?;
    let pinned = format!("+{revision}:refs/heads/poly-pinned/{revision}");
    let shallow = run_git(
        mirror,
        &["fetch", "--quiet", "--no-tags", "--depth", "1", "origin", &pinned],
    );
    if shallow.is_err() || !git_object_exists(mirror, revision)? {
        run_git(
            mirror,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "origin",
                "+refs/heads/*:refs/heads/*",
                "+refs/tags/*:refs/tags/*",
            ],
        )
        .with_context(|| format!("fetching locked source commit {revision} from {url}"))?;
    }
    if !git_object_exists(mirror, revision)? {
        bail!("locked source commit {revision} is unavailable from {url}");
    }
    Ok(())
}

/// Resolve `revision` (a branch, tag, other ref, or full object ID) on `url` to
/// the commit object ID it names, without downloading the repository.
///
/// A full object ID is returned unchanged and needs no network. Anything else is
/// looked up with `git ls-remote`, matched the way `git fetch` matches a ref
/// name, and an annotated tag is peeled to its commit.
pub fn resolve_remote_revision(url: &str, revision: &str) -> anyhow::Result<String> {
    if validate_locked_revision(revision).is_ok() {
        return Ok(revision.to_owned());
    }
    poly_config::network::require_online(&format!("resolve {revision} on {url}"))?;
    let peeled = format!("{revision}^{{}}");
    let output = git_command()
        .args(["ls-remote", "--", url, revision, &peeled])
        .output()
        .context("starting git ls-remote")?;
    if !output.status.success() {
        bail!(
            "git ls-remote {url} failed: {}",
            head_lines(&String::from_utf8_lossy(&output.stderr))
        );
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let resolved =
        pick_ls_remote_oid(&listing, revision).with_context(|| format!("revision {revision:?} not found on {url}"))?;
    validate_locked_revision(resolved)?;
    Ok(resolved.to_owned())
}

/// The object ID `git ls-remote` output names for `revision`, peeled.
///
/// Candidates are tried in `git fetch`'s own order (`<rev>`, `refs/<rev>`,
/// `refs/tags/<rev>`, `refs/heads/<rev>`, `refs/remotes/<rev>`,
/// `refs/remotes/<rev>/HEAD`), and only exact ref names count: `ls-remote`
/// patterns match ref-name suffixes, so `v5` also lists `refs/heads/release/v5`.
/// For the first candidate present, its `^{}` line — the commit an annotated tag
/// points at — wins over the ref's own object.
pub fn pick_ls_remote_oid<'a>(listing: &'a str, revision: &str) -> Option<&'a str> {
    let refs: Vec<(&str, &str)> = listing
        .lines()
        .filter_map(|line| {
            let (oid, name) = line.split_once('\t')?;
            Some((name.trim(), oid.trim()))
        })
        .collect();
    let lookup = |name: &str| {
        refs.iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, oid)| *oid)
    };
    [
        revision.to_owned(),
        format!("refs/{revision}"),
        format!("refs/tags/{revision}"),
        format!("refs/heads/{revision}"),
        format!("refs/remotes/{revision}"),
        format!("refs/remotes/{revision}/HEAD"),
    ]
    .iter()
    .find_map(|name| {
        let own = lookup(name)?;
        Some(lookup(&format!("{name}^{{}}")).unwrap_or(own))
    })
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

    const TAG_OBJECT: &str = "bf2bfe868338c42f576707ff6fd94f8d2892ee3e";
    const TAG_COMMIT: &str = "8c9be46d92a60d5b732d3d17ee4ded2a8863dfe9";
    const BRANCH: &str = "5cf12807a83f76882552503a51b1108bde33f257";
    const NESTED: &str = "1111111111111111111111111111111111111111";

    fn listing(lines: &[(&str, &str)]) -> String {
        lines.iter().map(|(oid, name)| format!("{oid}\t{name}\n")).collect()
    }

    #[test]
    fn ls_remote_annotated_tag_is_peeled_to_its_commit() {
        let output = listing(&[(TAG_OBJECT, "refs/tags/v5"), (TAG_COMMIT, "refs/tags/v5^{}")]);
        assert_eq!(pick_ls_remote_oid(&output, "v5"), Some(TAG_COMMIT));
    }

    #[test]
    fn ls_remote_lightweight_tag_and_branch_resolve_to_their_own_oid() {
        let output = listing(&[(TAG_COMMIT, "refs/tags/v5"), (BRANCH, "refs/heads/main")]);
        assert_eq!(pick_ls_remote_oid(&output, "v5"), Some(TAG_COMMIT));
        assert_eq!(pick_ls_remote_oid(&output, "main"), Some(BRANCH));
    }

    #[test]
    fn ls_remote_prefers_tag_over_branch_like_git_fetch() {
        let output = listing(&[
            (BRANCH, "refs/heads/v5"),
            (TAG_OBJECT, "refs/tags/v5"),
            (TAG_COMMIT, "refs/tags/v5^{}"),
        ]);
        assert_eq!(pick_ls_remote_oid(&output, "v5"), Some(TAG_COMMIT));
        assert_eq!(pick_ls_remote_oid(&output, "refs/heads/v5"), Some(BRANCH));
        assert_eq!(pick_ls_remote_oid(&output, "heads/v5"), Some(BRANCH));
    }

    #[test]
    fn ls_remote_ignores_suffix_matches_and_missing_refs() {
        // `git ls-remote <url> v5` also lists refs merely ending in `/v5`.
        let output = listing(&[(NESTED, "refs/heads/release/v5"), (BRANCH, "HEAD")]);
        assert_eq!(pick_ls_remote_oid(&output, "v5"), None);
        assert_eq!(pick_ls_remote_oid(&output, "HEAD"), Some(BRANCH));
        assert_eq!(pick_ls_remote_oid("", "main"), None);
    }

    #[test]
    fn full_object_id_resolves_without_the_network() {
        assert_eq!(
            resolve_remote_revision("https://example.invalid/never-contacted.git", TAG_COMMIT).unwrap(),
            TAG_COMMIT
        );
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
