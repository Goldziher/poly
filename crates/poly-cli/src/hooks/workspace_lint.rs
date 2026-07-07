//! The whole-project lint phase of `poly lint`.
//!
//! `poly lint`'s per-file tier (native engines + catalog tools) cannot run
//! *whole-project* analysis tools — `cargo clippy`, `cargo-sort`, `cargo-deny`,
//! type checkers — because they need a whole-workspace view that does not fit the
//! per-file rayon unit (ADR 0014). Those tools already have a home as
//! **whole-workspace hooks** (ADR 0019). This module bridges the two: it reuses
//! the hooks lowering to build exactly the same whole-workspace tool set, then
//! runs it as a phase of `poly lint` against the **live worktree** (no staged
//! snapshot — `poly lint` checks the working tree, not the index) and folds the
//! pass/fail into the lint report and exit code.
//!
//! The tool set and its toggles are the existing hooks config
//! (`[hooks.builtin.cargo]` + inline `workspace = true` jobs) — a single source
//! of truth, so `poly lint` runs the same whole-project tools a commit would.
//! The phase is on by default; `--no-workspace` or `[lint] workspace = false`
//! turns it off, and a repo that has not adopted `[hooks]` runs nothing here.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use owo_colors::{OwoColorize as _, Stream};

use crate::hooks::commands::{load_config, open_result_cache, sccache_settings, show_progress};
use crate::hooks::lower;

/// Inputs the whole-project lint phase needs from the `poly lint` invocation.
pub(crate) struct WorkspaceLintArgs<'a> {
    /// The `--config <path>` override, if any (else the nearest `poly.toml`).
    pub config: Option<&'a Path>,
    /// The `--no-workspace` flag: skip the phase entirely.
    pub no_workspace: bool,
    /// The `-j` concurrency override.
    pub jobs: Option<usize>,
    /// The `--no-cache` flag.
    pub no_cache: bool,
    /// Whether the human report goes to stdout (pretty) or stderr (json/toon, so
    /// stdout stays a single valid document).
    pub to_stdout: bool,
}

/// Run the whole-project lint phase and return `true` when it passed (or did not
/// run at all). A returned `false` means a whole-project tool reported failures,
/// which the caller folds into a non-zero exit code.
pub(crate) fn run(args: &WorkspaceLintArgs) -> Result<bool> {
    if args.no_workspace {
        return Ok(true);
    }
    let config = load_config(args.config)?;
    if workspace_lint_disabled(&config.lint) {
        return Ok(true);
    }

    // Whole-project tools run at the repository root; fall back to the working
    // directory for a non-git checkout so a standalone cargo project still works.
    let root = poly_hooks::git::get_root()
        .or_else(|_| std::env::current_dir())
        .context("failed to resolve the project root")?;
    let poly_bin = std::env::current_exe().context("failed to resolve the running poly binary")?;

    // Lower the pre-commit stage, then reduce it to just the whole-workspace
    // analysis hooks — the cargo builtins and inline `workspace = true` jobs.
    let mut spec = lower::lower_stage(
        &config.hooks,
        &poly_bin,
        poly_hooks::Stage::PreCommit,
        &[],
        &config.cache.results.hooks,
        &root,
        &config.tools,
    )?;
    retain_workspace_hooks(&mut spec);
    if spec.hooks.is_empty() {
        return Ok(true);
    }

    // Open the cache and resolve sccache before moving `root` into the request.
    let cache = open_result_cache(&config, &root, args.no_cache)?;
    let sccache = sccache_settings(&config, false)?;
    let request = poly_hooks::HookRunRequest {
        root,
        // No staged snapshot: `poly lint` analyses the live worktree, not the
        // index, so whole-project tools see the working-tree content directly.
        work_root: None,
        files: Vec::new(),
        message_file: None,
        stages: vec![spec],
        concurrency: args.jobs,
        cache,
        sccache,
        progress: show_progress(),
    };
    let outcome = poly_hooks::run(request)?;
    render(&outcome, args.to_stdout);
    Ok(outcome.success())
}

/// Reduce a lowered stage to just its whole-project analysis hooks.
///
/// Three adjustments make the lowered `pre-commit` stage safe to run as a lint
/// phase rather than a commit gate:
/// - keep only `workspace = true` hooks (the cargo builtins + inline whole-project
///   jobs); per-file hooks are `poly lint`'s own tier and are dropped here;
/// - force each retained hook to `always_run`, so a file-filtered inline job (e.g.
///   `files = "**/*.go"`) still runs against the whole project even though this
///   phase passes no candidate file list — otherwise it would be silently skipped
///   yet rendered as a pass;
/// - drop the stage's `precondition` / `before` / `after` scaffolding: `poly lint`
///   runs the tools, not the user's commit-time setup/teardown (a failing `before`
///   would otherwise abort with no rendered explanation).
fn retain_workspace_hooks(spec: &mut poly_hooks::StageSpec) {
    spec.hooks.retain(|hook| hook.workspace);
    for hook in &mut spec.hooks {
        hook.always_run = true;
    }
    spec.precondition = None;
    spec.before.clear();
    spec.after.clear();
}

/// `[lint] workspace = false` disables the whole-project phase. Any other value
/// (absent, `true`, or a non-boolean) leaves it enabled.
fn workspace_lint_disabled(lint: &toml::Table) -> bool {
    lint.get("workspace").and_then(toml::Value::as_bool) == Some(false)
}

/// Render the whole-project results under a lint-appropriate header — one
/// `✓/× id` line per tool, with each failing tool's captured output indented
/// beneath it. Written to stdout for pretty output, else stderr.
fn render(outcome: &poly_hooks::HookRunOutcome, to_stdout: bool) {
    let mut buffer = String::new();
    let mut any = false;
    for stage in &outcome.stages {
        for hook in &stage.hooks {
            any = true;
            let failed = hook.status.is_failure();
            let marker = status_marker(failed, to_stdout);
            let suffix = if hook.cached { " (cached)" } else { "" };
            buffer.push_str(&format!("  {marker} {}{suffix}\n", hook.id));
            if failed {
                append_output(&mut buffer, &hook.output);
            }
        }
    }
    if !any {
        return;
    }
    let header = "whole-project checks".if_supports_color(sink_stream(to_stdout), |t| t.bold());
    let block = format!("\n{header}\n{buffer}");
    if to_stdout {
        print!("{block}");
    } else {
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "{block}");
    }
}

/// A green `✓` / red `×` marker coloured against the stream it will print to.
fn status_marker(failed: bool, to_stdout: bool) -> String {
    let stream = sink_stream(to_stdout);
    if failed {
        "×".if_supports_color(stream, |t| t.red()).to_string()
    } else {
        "✓".if_supports_color(stream, |t| t.green()).to_string()
    }
}

fn sink_stream(to_stdout: bool) -> Stream {
    if to_stdout { Stream::Stdout } else { Stream::Stderr }
}

/// Append a failing tool's captured output, indented, ANSI stripped.
fn append_output(buffer: &mut String, output: &[u8]) {
    let text = String::from_utf8_lossy(output);
    let text = console::strip_ansi_codes(&text);
    for line in text.lines() {
        buffer.push_str("      ");
        buffer.push_str(line);
        buffer.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::{retain_workspace_hooks, workspace_lint_disabled};

    #[test]
    fn retain_keeps_workspace_hooks_forces_always_run_and_drops_steps() {
        use poly_hooks::{Hook, StageSpec};

        // A whole-project inline job with a file filter (so `always_run = false`)…
        let mut ws = Hook::run("go-vet", "go vet ./...");
        ws.workspace = true;
        ws.always_run = false;
        // …and a per-file hook that must be dropped (workspace defaults to false).
        let per_file = Hook::run("fmt", "poly fmt");

        let mut spec = StageSpec {
            precondition: Some("test -f Cargo.toml".to_string()),
            before: vec!["echo setup".to_string()],
            after: vec!["echo teardown".to_string()],
            hooks: vec![ws, per_file],
            ..StageSpec::default()
        };
        retain_workspace_hooks(&mut spec);

        assert_eq!(spec.hooks.len(), 1, "only the workspace hook is kept");
        assert_eq!(spec.hooks[0].id, "go-vet");
        assert!(
            spec.hooks[0].always_run,
            "a workspace lint hook must be forced always-run"
        );
        assert!(spec.precondition.is_none(), "commit-gate precondition is dropped");
        assert!(spec.before.is_empty(), "commit-gate before steps are dropped");
        assert!(spec.after.is_empty(), "commit-gate after steps are dropped");
    }

    #[test]
    fn workspace_disabled_only_on_explicit_false() {
        let disabled: toml::Table = toml::from_str("workspace = false").unwrap();
        assert!(workspace_lint_disabled(&disabled));

        let enabled: toml::Table = toml::from_str("workspace = true").unwrap();
        assert!(!workspace_lint_disabled(&enabled));

        let absent: toml::Table = toml::Table::new();
        assert!(!workspace_lint_disabled(&absent));

        // A non-boolean value must not silently disable the phase.
        let wrong_type: toml::Table = toml::from_str("workspace = \"no\"").unwrap();
        assert!(!workspace_lint_disabled(&wrong_type));
    }
}
