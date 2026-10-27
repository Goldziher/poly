//! Batch linting for `shellcheck` (ADR 0033 §6): one invocation over many files.
//!
//! `shellcheck` is the native-toolchain lint backend, and its per-file cost is
//! the same process startup `shfmt` pays — except that it is the heavier tool
//! (a Haskell runtime, ~40 ms to start, ~100 ms per script under load), so on a
//! repository with a few hundred scripts it is the largest single cost of
//! `poly lint`.
//!
//! # Why this does not change what is reported
//!
//! The ADR deferred this because handing shellcheck *real paths* changes its
//! answer: `source` directives start resolving, `.shellcheckrc` is found from the
//! file's directory instead of poly's, and a `.bash` name infers a dialect that
//! stdin never had. None of that is wanted from a performance change, so the
//! batch reproduces the stdin invocation instead:
//!
//! - the content is poly's own, written into a scratch mirror — never the real
//!   file, so the staged-snapshot guarantees of ADR 0019 hold;
//! - every member is named `input.sh`: shellcheck infers nothing from `.sh`,
//!   which is exactly what it infers from `-`;
//! - the `.shellcheckrc` poly's own working directory resolves is passed with
//!   `--rcfile`, because shellcheck would otherwise look for one beside the
//!   mirror;
//! - a configuration that makes shellcheck follow `source` (`external-sources`,
//!   `source-path`) declines the batch, since a mirror has nothing to follow.
//!
//! Equivalence with the stdin path is asserted on a real corpus, not argued.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;

use super::batch::{chunks_within_budget, materialize};
use super::isolate::{Isolation, Members};
use super::lint::{ShellcheckOutput, comment_to_diagnostic};
use super::spec::ToolSpec;
use super::tool_config::{SHELLCHECK_CONFIG_FILES, nearest_config_at};
use crate::engine::{Diagnostic, SourceFile};

/// The name every member is given, for the reason in the module docs.
const MIRROR_NAME: &str = "input.sh";

/// Config and environment spellings that make shellcheck resolve `source`
/// directives, which a mirror cannot satisfy.
const SOURCE_FOLLOWING_KEYS: &[&str] = &["external-sources", "source-path"];

/// Lint every member of `batch` with one `shellcheck` invocation per argv-sized
/// chunk, isolating any member the tool cannot process.
///
/// Returns one entry per input. An `Err` entry is a member to lint on its own;
/// the outer `Err` is a batch that must not be trusted at all.
pub(crate) fn lint_batch_via_shellcheck(
    spec: &ToolSpec,
    batch: &[SourceFile],
) -> anyhow::Result<Vec<anyhow::Result<Vec<Diagnostic>>>> {
    let binary = spec
        .lint_binary
        .context("lint_batch_via_shellcheck called on a format-only ToolSpec")?;
    if batch.is_empty() {
        return Ok(Vec::new());
    }
    let anchor = std::env::current_dir().context("reading the working directory for batch linting")?;
    let rc = nearest_config_at(&anchor, SHELLCHECK_CONFIG_FILES);
    if follows_sources(rc.as_deref(), std::env::var("SHELLCHECK_OPTS").ok().as_deref()) {
        anyhow::bail!("shellcheck is configured to follow `source`; not batching");
    }

    let scratch = tempfile::Builder::new()
        .prefix("poly-batch-")
        .tempdir()
        .context("creating scratch directory for batch linting")?;
    let mut run = Run {
        binary,
        args: spec
            .lint_args
            .iter()
            .filter(|arg| **arg != "-")
            .map(|arg| (*arg).to_owned())
            .collect(),
        rc,
        batch,
        mirrored: materialize(scratch.path(), batch, Some(MIRROR_NAME))?,
        pending: Vec::new(),
        outcomes: batch
            .iter()
            .map(|_| Err(anyhow::anyhow!("not linted in a batch")))
            .collect(),
    };
    let mut isolation = Isolation::default();
    for range in chunks_within_budget(&run.mirrored) {
        isolation.run(&mut run, range)?;
    }
    Ok(run.outcomes)
}

/// Whether the configuration in force asks shellcheck to follow `source`.
fn follows_sources(rc: Option<&Path>, options: Option<&str>) -> bool {
    let in_rc = rc
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|text| SOURCE_FOLLOWING_KEYS.iter().any(|key| text.contains(key)));
    let in_env = options.is_some_and(|text| {
        SOURCE_FOLLOWING_KEYS.iter().any(|key| text.contains(key))
            || text
                .split_whitespace()
                .any(|word| word == "-x" || word.starts_with("-P"))
    });
    in_rc || in_env
}

/// One batch in flight.
struct Run<'a> {
    binary: &'a str,
    args: Vec<String>,
    rc: Option<PathBuf>,
    batch: &'a [SourceFile],
    mirrored: Vec<PathBuf>,
    /// Findings of the last accepted attempt, by position within its range.
    pending: Vec<Vec<Diagnostic>>,
    outcomes: Vec<anyhow::Result<Vec<Diagnostic>>>,
}

impl Members for Run<'_> {
    fn attempt(&mut self, range: &std::ops::Range<usize>) -> anyhow::Result<bool> {
        for index in range.clone() {
            std::fs::write(&self.mirrored[index], self.batch[index].content.as_bytes())
                .with_context(|| format!("writing batch input for {}", self.batch[index].path.display()))?;
        }
        let mut command = Command::new(self.binary);
        command.args(&self.args);
        if let Some(rc) = &self.rc {
            command.arg(format!("--rcfile={}", rc.display()));
        }
        let output = command
            .args(&self.mirrored[range.clone()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .with_context(|| format!("failed to run '{}' over a batch", self.binary))?;
        // 0 and 1 are "scanned" (clean / findings). Anything higher means a file
        // could not be processed or the invocation was refused, and the JSON, if
        // any, is not an answer for every member.
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Ok(false);
        }
        let Ok(parsed) = serde_json::from_slice::<ShellcheckOutput>(&output.stdout) else {
            return Ok(false);
        };
        let mut pending: Vec<Vec<Diagnostic>> = range.clone().map(|_| Vec::new()).collect();
        for comment in parsed.comments {
            let Some(offset) = self.mirrored[range.clone()]
                .iter()
                .position(|path| path.as_os_str() == comment.file.as_str())
            else {
                return Ok(false);
            };
            pending[offset].push(comment_to_diagnostic("shellcheck", comment));
        }
        self.pending = pending;
        Ok(true)
    }

    fn accept(&mut self, range: &std::ops::Range<usize>) {
        for (index, diagnostics) in range.clone().zip(std::mem::take(&mut self.pending)) {
            self.outcomes[index] = Ok(diagnostics);
        }
    }

    fn refuse(&mut self, index: usize) {
        self.outcomes[index] = Err(anyhow::anyhow!(
            "'{}' refused {} in a batch; linting it on its own",
            self.binary,
            self.batch[index].path.display()
        ));
    }

    fn tool(&self) -> &str {
        self.binary
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::lint::lint_via_shellcheck;
    use super::super::spec::SHELLCHECK_SPEC;
    use super::*;
    use crate::language::Language;

    fn shell(path: &str, content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            language: Language::Shell,
            content: Arc::from(content),
        }
    }

    fn shellcheck_present() -> bool {
        Command::new("shellcheck")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// The claim the module rests on: a batch reports exactly what the stdin
    /// invocation reports, member by member, in the same order. The corpus
    /// deliberately includes a file with no shebang under a `.bash` name — the
    /// one place real paths would have diverged — and one with findings on
    /// several lines, so a swapped or merged attribution cannot pass.
    #[test]
    fn a_batch_reports_what_the_stdin_path_reports_for_every_member() {
        if !shellcheck_present() {
            eprintln!("skipping: shellcheck not on PATH");
            return;
        }
        let batch = vec![
            shell("/repo/clean.sh", "#!/bin/sh\necho \"ok\"\n"),
            shell("/repo/unquoted.bash", "echo $1\n"),
            shell("/repo/many.sh", "#!/bin/bash\ncd $1\nrm -rf $2\nls $3\n"),
            shell("/repo/unparseable.sh", "#!/bin/sh\nif true; then\n"),
        ];

        let batched = lint_batch_via_shellcheck(&SHELLCHECK_SPEC, &batch).expect("the batch is trusted");

        assert_eq!(batched.len(), batch.len());
        for (src, got) in batch.iter().zip(&batched) {
            let want = lint_via_shellcheck(&SHELLCHECK_SPEC, src).expect("stdin path");
            let got = got.as_ref().expect("every member was answered");
            assert_eq!(
                serde_json::to_value(got).expect("diagnostics serialize"),
                serde_json::to_value(&want).expect("diagnostics serialize"),
                "{} differs between the batch and the stdin path",
                src.path.display()
            );
        }
        assert!(
            batched[2].as_ref().expect("answered").len() >= 3,
            "the multi-finding member must keep every finding"
        );
    }

    #[test]
    fn source_following_configuration_declines_the_batch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rc = dir.path().join(".shellcheckrc");
        std::fs::write(&rc, "external-sources=true\n").expect("write rc");
        assert!(follows_sources(Some(&rc), None));
        std::fs::write(&rc, "disable=SC2086\n").expect("write rc");
        assert!(!follows_sources(Some(&rc), None));
        assert!(follows_sources(None, Some("--external-sources")));
        assert!(follows_sources(None, Some("-x --severity=error")));
        assert!(!follows_sources(None, Some("--severity=error")));
        assert!(!follows_sources(None, None));
    }
}
