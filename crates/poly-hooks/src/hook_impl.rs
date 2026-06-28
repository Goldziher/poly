//! Per-stage git stdin / argument parsing for the installed hook shim.
//!
//! Ported (and made synchronous) from `polyhooks/src/cli/hook_impl.rs`. When a
//! git hook fires, the installed shim execs `poly hooks hook-impl
//! --hook-type=<type> -- <git args>`. This module turns the `<git args>` (and,
//! for `pre-push`, the stdin payload) into [`RunInputs`] — the resolved file /
//! ref / message-file inputs the caller (`poly-cli`) folds into a
//! [`crate::HookRunRequest`] alongside the lowered `Vec<StageSpec>` (B3).
//!
//! Out of scope (handled elsewhere or deliberately dropped): legacy-hook
//! *execution* chaining / migration mode, env provisioning, config discovery,
//! and store/workspace logic. `poly-cli`'s clap layer parses `--hook-type` and
//! the trailing `--` args, then calls [`hook_impl`].

use std::ffi::{OsStr, OsString};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::git;
use crate::stage::{HookType, RunInputMode, Stage};

/// The resolved inputs derived from a git hook invocation.
///
/// The caller combines these with the lowered stage specs to build a
/// [`crate::HookRunRequest`]. The `from_ref` / `to_ref` / `all_files` triple
/// drives how the candidate file set is computed (range diff vs. whole tree);
/// `message_file` carries the commit-message path for the message stages. The
/// remaining fields preserve the extra git arguments verbatim for callers that
/// surface them (they do not affect file selection).
#[derive(Debug, Clone, Default)]
pub struct RunInputs {
    /// The hook type that fired.
    pub hook_type: HookType,
    /// The [`Stage`] corresponding to `hook_type`.
    pub stage: Stage,
    /// Commit-message file path (`commit-msg` / `prepare-commit-msg`).
    pub message_file: Option<PathBuf>,
    /// Lower bound of the diff range (the remote/old ref), if any.
    pub from_ref: Option<String>,
    /// Upper bound of the diff range (the local/new ref), if any.
    pub to_ref: Option<String>,
    /// Run against the whole tracked tree rather than a diff range.
    pub all_files: bool,
    /// `pre-push`: the remote name (`argv[0]`).
    pub remote_name: Option<String>,
    /// `pre-push`: the remote URL (`argv[1]`).
    pub remote_url: Option<String>,
    /// `pre-push`: the remote branch being updated.
    pub remote_branch: Option<String>,
    /// `pre-push`: the local branch being pushed.
    pub local_branch: Option<String>,
    /// `post-checkout`: the checkout flag (`1` = branch checkout).
    pub checkout_type: Option<String>,
    /// `post-merge`: whether the merge was a squash merge.
    pub is_squash_merge: bool,
    /// `post-rewrite`: the command that triggered the rewrite (`amend` / `rebase`).
    pub rewrite_command: Option<String>,
    /// `prepare-commit-msg`: the message source (`message` / `template` / …).
    pub prepare_commit_message_source: Option<String>,
    /// `prepare-commit-msg`: the commit object name, when supplied.
    pub commit_object_name: Option<String>,
    /// `pre-rebase`: the upstream the branch was forked from.
    pub pre_rebase_upstream: Option<String>,
    /// `pre-rebase`: the branch being rebased, when supplied.
    pub pre_rebase_branch: Option<String>,
}

impl RunInputs {
    /// Whether this stage consumes a file list, a message file, or no files.
    #[must_use]
    pub fn input_mode(&self) -> RunInputMode {
        RunInputMode::from(self.stage)
    }
}

/// The number of git arguments each hook type expects.
///
/// Mirrors the contract in <https://git-scm.com/docs/githooks>.
#[must_use]
pub fn hook_num_args(hook_type: HookType) -> RangeInclusive<usize> {
    match hook_type {
        HookType::PostCheckout => 3..=3,
        HookType::PreCommit | HookType::PostCommit | HookType::PreMergeCommit => 0..=0,
        HookType::CommitMsg | HookType::PostMerge | HookType::PostRewrite => 1..=1,
        HookType::PrePush => 2..=2,
        HookType::PreRebase => 1..=2,
        HookType::PrepareCommitMsg => 1..=3,
    }
}

/// Read the hook's stdin — only `pre-push` receives a payload; every other hook
/// gets an empty buffer.
pub fn read_hook_stdin(hook_type: HookType) -> Result<Vec<u8>> {
    use std::io::Read as _;

    if !matches!(hook_type, HookType::PrePush) {
        return Ok(Vec::new());
    }
    let mut buffer = Vec::new();
    std::io::stdin().read_to_end(&mut buffer)?;
    Ok(buffer)
}

/// The resolved push range for a single `pre-push` stdin line.
#[derive(Debug, Clone)]
pub struct PushInfo {
    /// Diff lower bound (old remote ref), if any.
    pub from_ref: Option<String>,
    /// Diff upper bound (new local ref), if any.
    pub to_ref: Option<String>,
    /// Run against the whole tree (root-commit push), rather than a range.
    pub all_files: bool,
    /// The remote branch being updated.
    pub remote_branch: Option<String>,
    /// The local branch being pushed.
    pub local_branch: Option<String>,
}

/// Parse `pre-push` stdin into the range of commits to check.
///
/// Each stdin line is `<local-ref> <local-sha> <remote-ref> <remote-sha>`. Three
/// cases are reproduced from upstream:
///
/// 1. **Normal update** — the old remote tip exists and is an ancestor of the
///    new local tip: diff exactly `remote_sha..local_sha`.
/// 2. **Rebase / force-push** — the old remote tip is missing or not an
///    ancestor: diff from the parent of the first commit the remote cannot
///    reach.
/// 3. **New branch** — that first remote-unknown commit is itself a root
///    commit: there is no parent, so check the whole tracked tree.
///
/// Returns `Ok(None)` when no line describes anything to push (deletions,
/// already-reachable tips, malformed input).
pub fn parse_pre_push_info(
    stdin: &[u8],
    remote_name: &str,
    root: &Path,
) -> Result<Option<PushInfo>> {
    let buffer = String::from_utf8_lossy(stdin);

    for line in buffer.lines() {
        // `rsplitn(4, ' ')` yields the fields in reverse: remote_sha, remote_ref,
        // local_sha, local_ref. Refs may contain spaces only on the right side
        // here, so splitting from the right keeps the SHAs intact.
        let parts: Vec<&str> = line.rsplitn(4, ' ').collect();
        if parts.len() != 4 {
            // Ignore malformed lines; a later valid line may still describe a push.
            continue;
        }

        let local_branch = parts[3];
        let local_sha = parts[2];
        let remote_branch = parts[1];
        let remote_sha = parts[0];

        // A zero local SHA means this push deletes the remote ref. There is no
        // local target commit to diff, so it contributes no files to check.
        if local_sha.bytes().all(|b| b == b'0') {
            continue;
        }

        if !remote_sha.bytes().all(|b| b == b'0') && git::rev_exists(remote_sha, root)? {
            if git::is_ancestor(remote_sha, local_sha, root)? {
                // Normal update: the previous remote tip is an ancestor of the
                // new local tip, so diff exactly the newly pushed range.
                return Ok(Some(PushInfo {
                    from_ref: Some(remote_sha.to_string()),
                    to_ref: Some(local_sha.to_string()),
                    all_files: false,
                    remote_branch: Some(remote_branch.to_string()),
                    local_branch: Some(local_branch.to_string()),
                }));
            }
            // The old remote tip exists locally but is not in the new local
            // history (the usual rebase/force-push shape). Fall through to the
            // new-branch logic to derive a PR-like base.
        }

        // New remote ref, missing old remote object, or rebased force-push: find
        // the commits reachable from the local tip the remote cannot reach.
        let ancestors = git::get_ancestors_not_in_remote(local_sha, remote_name, root)?;
        let Some(first_ancestor) = ancestors.first() else {
            // The local tip is already reachable from the remote.
            continue;
        };

        let roots = git::get_root_commits(local_sha, root)?;
        if roots.contains(first_ancestor) {
            // The first commit being pushed is a root commit: no parent to use
            // as `from_ref`, so run over the full tracked tree.
            return Ok(Some(PushInfo {
                from_ref: None,
                to_ref: Some(local_sha.to_string()),
                all_files: true,
                remote_branch: Some(remote_branch.to_string()),
                local_branch: Some(local_branch.to_string()),
            }));
        }

        // Use the parent of the first remote-unknown commit as the diff base.
        if let Some(source) = git::get_parent_commit(first_ancestor, root)? {
            return Ok(Some(PushInfo {
                from_ref: Some(source),
                to_ref: Some(local_sha.to_string()),
                all_files: false,
                remote_branch: Some(remote_branch.to_string()),
                local_branch: Some(local_branch.to_string()),
            }));
        }

        // A non-root commit should have a parent. If git cannot provide one,
        // ignore this line and let a later line determine the range.
    }

    // Nothing to push.
    Ok(None)
}

/// Convert an `OsStr` argument to an owned `String` (lossily).
fn arg_str(arg: &OsStr) -> String {
    arg.to_string_lossy().into_owned()
}

/// Resolve the hook invocation into [`RunInputs`].
///
/// `args` are the git arguments after the shim's `--`; `stdin` is the hook's
/// stdin (empty for everything but `pre-push`). Returns `Ok(None)` when there is
/// nothing to do (a `pre-push` with nothing to push). Splitting the stdin read
/// out of this function keeps it unit-testable.
pub fn resolve_inputs(
    hook_type: HookType,
    args: &[OsString],
    stdin: &[u8],
    root: &Path,
) -> Result<Option<RunInputs>> {
    let expected = hook_num_args(hook_type);
    if !expected.contains(&args.len()) {
        bail!(
            "hook `{hook_type}` expects {} but received {}{}",
            format_expected_args(&expected),
            format_received_args(args.len()),
            format_argument_dump(args),
        );
    }

    let mut inputs = RunInputs {
        hook_type,
        stage: Stage::from(hook_type),
        ..RunInputs::default()
    };

    match hook_type {
        HookType::PrePush => {
            let remote_name = arg_str(&args[0]);
            inputs.remote_url = Some(arg_str(&args[1]));
            let Some(push) = parse_pre_push_info(stdin, &remote_name, root)? else {
                return Ok(None);
            };
            inputs.remote_name = Some(remote_name);
            inputs.from_ref = push.from_ref;
            inputs.to_ref = push.to_ref;
            inputs.all_files = push.all_files;
            inputs.remote_branch = push.remote_branch;
            inputs.local_branch = push.local_branch;
        }
        HookType::CommitMsg => {
            inputs.message_file = Some(PathBuf::from(&args[0]));
        }
        HookType::PrepareCommitMsg => {
            inputs.message_file = Some(PathBuf::from(&args[0]));
            if args.len() > 1 {
                inputs.prepare_commit_message_source = Some(arg_str(&args[1]));
            }
            if args.len() > 2 {
                inputs.commit_object_name = Some(arg_str(&args[2]));
            }
        }
        HookType::PostCheckout => {
            inputs.from_ref = Some(arg_str(&args[0]));
            inputs.to_ref = Some(arg_str(&args[1]));
            inputs.checkout_type = Some(arg_str(&args[2]));
        }
        HookType::PostMerge => {
            inputs.is_squash_merge = args[0].to_string_lossy() == "1";
        }
        HookType::PostRewrite => {
            inputs.rewrite_command = Some(arg_str(&args[0]));
        }
        HookType::PreRebase => {
            inputs.pre_rebase_upstream = Some(arg_str(&args[0]));
            if args.len() > 1 {
                inputs.pre_rebase_branch = Some(arg_str(&args[1]));
            }
        }
        HookType::PreCommit | HookType::PostCommit | HookType::PreMergeCommit => {}
    }

    Ok(Some(inputs))
}

/// Entry point invoked by `poly hooks hook-impl`: read the hook's stdin and
/// resolve the git arguments into [`RunInputs`].
///
/// `root` is the repository root the hooks run in. Returns `Ok(None)` when there
/// is nothing to do (a `pre-push` with nothing to push).
pub fn hook_impl(hook_type: HookType, args: &[OsString], root: &Path) -> Result<Option<RunInputs>> {
    let stdin = read_hook_stdin(hook_type)?;
    resolve_inputs(hook_type, args, &stdin, root)
}

fn format_expected_args(range: &RangeInclusive<usize>) -> String {
    let (start, end) = (*range.start(), *range.end());
    match (start, end) {
        (0, 0) => "no arguments".to_string(),
        (1, 1) => "exactly 1 argument".to_string(),
        (s, e) if s == e => format!("exactly {s} arguments"),
        (0, e) => format!("up to {e} arguments"),
        (s, usize::MAX) => format!("at least {s} arguments"),
        (s, e) => format!("between {s} and {e} arguments"),
    }
}

fn format_received_args(received: usize) -> String {
    match received {
        0 => "no arguments".to_string(),
        1 => "1 argument".to_string(),
        n => format!("{n} arguments"),
    }
}

fn format_argument_dump(args: &[OsString]) -> String {
    if args.is_empty() {
        String::new()
    } else {
        let joined = args
            .iter()
            .map(|s| s.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        format!(": `{joined}`")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    use tempfile::TempDir;

    fn git_run(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git invocation");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init_temp_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path();
        git_run(path, &["init", "-q"]);
        git_run(path, &["config", "user.email", "test@example.com"]);
        git_run(path, &["config", "user.name", "Test"]);
        git_run(path, &["config", "commit.gpgsign", "false"]);
        dir
    }

    fn commit_file(repo: &Path, name: &str) -> String {
        std::fs::write(repo.join(name), name).expect("write file");
        git_run(repo, &["add", name]);
        git_run(repo, &["commit", "-q", "-m", name]);
        git_run(repo, &["rev-parse", "HEAD"])
    }

    #[test]
    fn parse_pre_push_normal_update_diffs_the_pushed_range() {
        let repo = init_temp_repo();
        let root = repo.path();
        let old_remote = commit_file(root, "a.txt");
        let new_local = commit_file(root, "b.txt");

        // `<local-ref> <local-sha> <remote-ref> <remote-sha>`
        let line = format!("refs/heads/main {new_local} refs/heads/main {old_remote}\n");
        let push = parse_pre_push_info(line.as_bytes(), "origin", root)
            .expect("parse")
            .expect("a push");

        assert_eq!(push.from_ref.as_deref(), Some(old_remote.as_str()));
        assert_eq!(push.to_ref.as_deref(), Some(new_local.as_str()));
        assert!(!push.all_files);
        assert_eq!(push.local_branch.as_deref(), Some("refs/heads/main"));
        assert_eq!(push.remote_branch.as_deref(), Some("refs/heads/main"));
    }

    #[test]
    fn parse_pre_push_new_branch_checks_whole_tree() {
        let repo = init_temp_repo();
        let root = repo.path();
        let _root_commit = commit_file(root, "a.txt");
        let new_local = commit_file(root, "b.txt");

        // New branch: the remote SHA is all zeros.
        let zero = "0".repeat(40);
        let line = format!("refs/heads/feature {new_local} refs/heads/feature {zero}\n");
        let push = parse_pre_push_info(line.as_bytes(), "origin", root)
            .expect("parse")
            .expect("a push");

        assert!(
            push.all_files,
            "new-branch push should check the whole tree"
        );
        assert_eq!(push.from_ref, None);
        assert_eq!(push.to_ref.as_deref(), Some(new_local.as_str()));
    }

    #[test]
    fn parse_pre_push_deletion_is_skipped() {
        let repo = init_temp_repo();
        let root = repo.path();
        commit_file(root, "a.txt");

        // Deletion: the local SHA is all zeros.
        let zero = "0".repeat(40);
        let line = format!("(delete) {zero} refs/heads/gone {zero}\n");
        let push = parse_pre_push_info(line.as_bytes(), "origin", root).expect("parse");
        assert!(push.is_none());
    }

    #[test]
    fn resolve_inputs_commit_msg_carries_message_file() {
        let repo = init_temp_repo();
        let args = vec![OsString::from("/tmp/COMMIT_EDITMSG")];
        let inputs = resolve_inputs(HookType::CommitMsg, &args, &[], repo.path())
            .expect("resolve")
            .expect("inputs");
        assert_eq!(
            inputs.message_file.as_deref(),
            Some(Path::new("/tmp/COMMIT_EDITMSG"))
        );
        assert_eq!(inputs.stage, Stage::CommitMsg);
        assert_eq!(inputs.input_mode(), RunInputMode::MessageFile);
    }

    #[test]
    fn resolve_inputs_rejects_wrong_argument_count() {
        let repo = init_temp_repo();
        // commit-msg expects exactly 1 argument; supply none.
        let err =
            resolve_inputs(HookType::CommitMsg, &[], &[], repo.path()).expect_err("should reject");
        assert!(err.to_string().contains("expects exactly 1 argument"));
    }
}
