//! The whole-project lint phase of `poly lint`, as a front-end-agnostic API.
//!
//! `poly lint`'s per-file tier (native engines + catalog tools) cannot run
//! *whole-project* analysis tools — `cargo clippy`, `cargo-sort`, `cargo-deny`,
//! type checkers — because they need a whole-workspace view that does not fit the
//! per-file rayon unit (ADR 0014). Those tools already have a home as
//! **whole-workspace hooks** (ADR 0019). This module bridges the two: it reuses
//! the hooks lowering ([`crate::lower`]) to build exactly the same whole-workspace
//! tool set, then runs it against the **live worktree** (no staged snapshot —
//! `poly lint` checks the working tree, not the index) and returns the pass/fail
//! plus the structured per-tool results.
//!
//! **Check mode is not read-only.** Without [`WorkspaceLintOptions::fix`] this
//! phase asks each tool for its check mode and applies no fixes of its own — but
//! it still *executes* them against the live worktree, and their own side effects
//! are outside poly's control: `cargo clippy` populates `target/` and can refresh
//! `Cargo.lock`, a `go` invocation can append to `go.work.sum`, a type checker
//! writes its cache. A caller that must leave the tree untouched has to skip this
//! phase entirely, not run it in check mode.
//!
//! The tool set and its toggles are the existing hooks config
//! (`[hooks.builtin.cargo]` + inline `workspace = true` jobs) — a single source
//! of truth, so `poly lint` runs the same whole-project tools a commit would.
//!
//! Config is **injected**, not loaded here: the caller passes a resolved
//! [`PolyConfig`], keeping front-end-specific `extends` resolution (the CLI's
//! git-remote resolver vs the MCP server's network-free one) out of this crate.
//! The CLI renders the outcome with [`render_workspace_outcome`]; a non-CLI
//! caller can serialize [`WorkspaceLintOutcome`] directly instead.

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};
use owo_colors::{OwoColorize as _, Stream};
use poly_config::PolyConfig;

use crate::lower;
use crate::support::{open_result_cache, sccache_settings, show_progress};

/// Per-invocation inputs the whole-project lint phase needs from the caller.
///
/// Everything else (the tool set, its toggles, the cache policy) comes from the
/// injected [`PolyConfig`]. The `--no-workspace` short-circuit is the caller's
/// responsibility: skip calling [`run_workspace_lint`] entirely rather than
/// threading a flag through here.
#[derive(Debug, Clone, Copy, Default)]
pub struct WorkspaceLintOptions {
    /// Apply autofixes: run the whole-project tools (`cargo sort`,
    /// `cargo-machete`, `cargo clippy`) in their fix mode rather than
    /// check-only. Set from `--fix`; the git-hook / commit-gate path never
    /// enables this.
    ///
    /// "Check-only" means poly requests no fixes — not that the tree is left
    /// alone. The tools run either way and may write on their own (see the module
    /// docs); only skipping the phase guarantees an untouched worktree.
    pub fix: bool,
    /// The `-j` concurrency override.
    pub jobs: Option<usize>,
    /// The `--no-cache` flag.
    pub no_cache: bool,
    /// Whether the human report goes to stdout (pretty) or stderr (json/toon, so
    /// stdout stays a single valid document). Controls only rendering and the
    /// colour decision — never which tools run.
    pub report_to_stdout: bool,
}

/// One whole-project tool's result, kept structured so a non-CLI caller (the MCP
/// server) can serialize it. Mirrors what [`render_workspace_outcome`] prints.
#[derive(Debug, Clone)]
pub struct WorkspaceToolResult {
    /// The hook id (e.g. `cargo-clippy`, or an inline job's label).
    pub id: String,
    /// Whether the tool reported a failure (its captured output is meaningful).
    pub failed: bool,
    /// Whether this result was served from the result cache.
    pub cached: bool,
    /// The tool's captured combined output (may contain ANSI colour codes).
    pub output: Vec<u8>,
}

/// The outcome of the whole-project lint phase: the overall pass/fail plus each
/// tool's structured result.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceLintOutcome {
    /// `true` when the phase passed (or did not run at all). `false` means a
    /// whole-project tool reported failures — the caller folds this into a
    /// non-zero exit code.
    pub passed: bool,
    /// The per-tool results, in run order. Empty when the phase did not run
    /// (disabled, or no whole-project tools configured).
    pub tools: Vec<WorkspaceToolResult>,
}

impl WorkspaceLintOutcome {
    /// The outcome for a phase that did not run: passed, with no tool results.
    fn skipped() -> Self {
        Self {
            passed: true,
            tools: Vec::new(),
        }
    }

    /// Flatten a [`poly_hooks::HookRunOutcome`] into the structured outcome.
    fn from_hook_outcome(outcome: &poly_hooks::HookRunOutcome) -> Self {
        let mut tools = Vec::new();
        for stage in &outcome.stages {
            for hook in &stage.hooks {
                tools.push(WorkspaceToolResult {
                    id: hook.id.clone(),
                    failed: hook.status.is_failure(),
                    cached: hook.cached,
                    output: hook.output.clone(),
                });
            }
        }
        Self {
            passed: outcome.success(),
            tools,
        }
    }
}

/// Run the whole-project lint phase against the live worktree and return the
/// structured outcome.
///
/// `config` is the already-resolved poly configuration (the caller owns
/// `extends` resolution). The phase is off when `[lint] workspace = false` or
/// when the repo configures no whole-project tools — both return a passing,
/// empty [`WorkspaceLintOutcome`]. The caller handles the `--no-workspace`
/// short-circuit before calling.
///
/// # Errors
///
/// Returns `Err` if the project root or running binary cannot be resolved, the
/// hooks config fails to lower, or the cache/runner setup fails.
pub fn run_workspace_lint(config: &PolyConfig, opts: &WorkspaceLintOptions) -> Result<WorkspaceLintOutcome> {
    let Some((root, mut spec)) = planned_workspace_stage(config)? else {
        return Ok(WorkspaceLintOutcome::skipped());
    };
    if opts.fix {
        lower::apply_cargo_fix_mode(&mut spec);
    }

    // When poly's own output is coloured, make the captured tools emit colour too
    // (they otherwise see a capture pipe, not a TTY, and self-disable it). Gated on
    // the same decision as the report below, so `--no-color`/redirected output stays
    // clean. Paired with the pass-through in `append_output`.
    let color = color_enabled(opts.report_to_stdout);
    if color {
        force_child_color(&mut spec);
    }

    let cache = open_result_cache(config, &root, opts.no_cache)?;
    let sccache = sccache_settings(config, false)?;
    let request = poly_hooks::HookRunRequest {
        root,
        work_root: None,
        files: Vec::new(),
        message_file: None,
        stages: vec![spec],
        concurrency: opts.jobs,
        cache,
        sccache,
        progress: show_progress(),
    };
    let outcome = poly_hooks::run(request)?;
    Ok(WorkspaceLintOutcome::from_hook_outcome(&outcome))
}

/// The ids of the whole-project tools this phase **would** run, without running
/// any of them. Empty when the phase would not run at all.
///
/// `poly lint` renders its per-file report before the whole-project phase has
/// executed, yet the per-file tier has to know which languages that phase covers
/// *first* — otherwise it reports `no lint rules for Rust` against the very
/// files `cargo clippy` is about to lint, and one run contradicts itself. This
/// answers from the same lowering and the same retain filter
/// [`run_workspace_lint`] uses, so the prediction and the run cannot drift into
/// disagreeing about which tools are in play.
///
/// # Errors
///
/// Returns `Err` for the same reasons [`run_workspace_lint`] does: the project
/// root or running binary cannot be resolved, or the hooks config fails to lower.
pub fn planned_workspace_tool_ids(config: &PolyConfig) -> Result<Vec<String>> {
    Ok(planned_workspace_stage(config)?
        .map(|(_, spec)| spec.hooks.into_iter().map(|hook| hook.id).collect())
        .unwrap_or_default())
}

/// Lower the `pre-commit` stage and reduce it to the whole-project tools that
/// would run, paired with the project root they run against.
///
/// `None` means the phase does not run: `[lint] workspace = false`, or a repo
/// that configures no whole-project tools at all. The root is resolved only once
/// past the disabled check, so a repo that has turned the phase off never pays
/// for the `git rev-parse` that finding it costs.
fn planned_workspace_stage(config: &PolyConfig) -> Result<Option<(PathBuf, poly_hooks::StageSpec)>> {
    if workspace_lint_disabled(&config.lint) {
        return Ok(None);
    }
    let root = poly_hooks::git::get_root()
        .or_else(|_| std::env::current_dir())
        .context("failed to resolve the project root")?;
    let poly_bin = std::env::current_exe().context("failed to resolve the running poly binary")?;
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
    Ok((!spec.hooks.is_empty()).then_some((root, spec)))
}

/// Force colour from the captured whole-project tools by setting the standard
/// force-colour env vars on each hook (cargo tools honour `CARGO_TERM_COLOR`;
/// `CLICOLOR_FORCE` / `FORCE_COLOR` cover the broader ecosystem). A user-set value
/// wins, so explicit config is never overridden.
fn force_child_color(spec: &mut poly_hooks::StageSpec) {
    const FORCE_COLOR: &[(&str, &str)] = &[
        ("CARGO_TERM_COLOR", "always"),
        ("CLICOLOR_FORCE", "1"),
        ("FORCE_COLOR", "1"),
    ];
    for hook in &mut spec.hooks {
        for (key, value) in FORCE_COLOR {
            hook.env.entry((*key).to_owned()).or_insert_with(|| (*value).to_owned());
        }
    }
}

/// Whether coloured output is enabled for the sink the report prints to. Matches
/// the decision owo-colors makes for poly's own markers — honouring `--no-color`
/// (global override), `NO_COLOR`/`CLICOLOR`, and per-stream TTY detection — by
/// probing it directly, so the force-colour and ANSI-passthrough paths stay in
/// lock-step with the rest of the report's colouring.
fn color_enabled(to_stdout: bool) -> bool {
    // If colour is on, the wrapper injects ANSI around the sentinel, so it differs.
    format!("{}", 'x'.if_supports_color(sink_stream(to_stdout), |t| t.red())) != "x"
}

/// Reduce a lowered stage to just its whole-project analysis hooks.
///
/// Three adjustments make the lowered `pre-commit` stage safe to run as a lint
/// phase rather than a commit gate:
/// - keep only `workspace = true` hooks (the cargo builtins + inline whole-project
///   jobs) that have not opted out via `skip_in_lint` (e.g. `[hooks.builtin.cargo]
///   lint = false`); per-file hooks are `poly lint`'s own tier and are dropped here;
/// - force each retained hook to `always_run`, so a file-filtered inline job (e.g.
///   `files = "**/*.go"`) still runs against the whole project even though this
///   phase passes no candidate file list — otherwise it would be silently skipped
///   yet rendered as a pass;
/// - drop the **stage's** `precondition` / `before` / `after` scaffolding: `poly
///   lint` runs the tools, not the user's commit-time setup/teardown. A hook's
///   *own* `precondition`/`before` is kept — it belongs to the tool, not to the
///   commit gate, and dropping it would run a tool whose prerequisite is unmet.
fn retain_workspace_hooks(spec: &mut poly_hooks::StageSpec) {
    spec.hooks.retain(|hook| hook.workspace && !hook.skip_in_lint);
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
/// beneath it. Written to stdout when `report_to_stdout`, else stderr.
///
/// Byte-for-byte identical to the historic in-CLI renderer. A phase that did not
/// run (empty [`WorkspaceLintOutcome::tools`]) prints nothing.
pub fn render_workspace_outcome(outcome: &WorkspaceLintOutcome, report_to_stdout: bool) {
    if outcome.tools.is_empty() {
        return;
    }
    let color = color_enabled(report_to_stdout);
    let mut buffer = String::new();
    for tool in &outcome.tools {
        let marker = status_marker(tool.failed, report_to_stdout);
        let suffix = if tool.cached { " (cached)" } else { "" };
        buffer.push_str(&format!("  {marker} {}{suffix}\n", tool.id));
        if tool.failed {
            append_output(&mut buffer, &tool.output, color);
        }
    }
    let header = "whole-project checks".if_supports_color(sink_stream(report_to_stdout), |t| t.bold());
    let block = format!("\n{header}\n{buffer}");
    if report_to_stdout {
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

/// Append a failing tool's captured output, indented. ANSI colour codes are kept
/// when `color` is set (poly's output is a colour-capable terminal) and stripped
/// otherwise, so redirected/`--no-color` output stays plain.
fn append_output(buffer: &mut String, output: &[u8], color: bool) {
    let raw = String::from_utf8_lossy(output);
    let text = if color {
        raw
    } else {
        std::borrow::Cow::Owned(console::strip_ansi_codes(&raw).into_owned())
    };
    for line in text.lines() {
        buffer.push_str("      ");
        buffer.push_str(line);
        buffer.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::{append_output, force_child_color, retain_workspace_hooks, workspace_lint_disabled};

    const RED_HELLO: &str = "\x1b[31mhello\x1b[0m";

    #[test]
    fn append_output_strips_ansi_when_color_off() {
        let mut buffer = String::new();
        append_output(&mut buffer, RED_HELLO.as_bytes(), false);
        assert_eq!(buffer, "      hello\n", "ANSI must be stripped when colour is off");
    }

    #[test]
    fn append_output_keeps_ansi_when_color_on() {
        let mut buffer = String::new();
        append_output(&mut buffer, RED_HELLO.as_bytes(), true);
        assert_eq!(
            buffer, "      \x1b[31mhello\x1b[0m\n",
            "ANSI must pass through when colour is on"
        );
    }

    #[test]
    fn force_child_color_sets_vars_without_overriding_user() {
        use poly_hooks::{Hook, StageSpec};

        let plain = Hook::run("cargo-clippy", "cargo clippy");
        let mut preset = Hook::run("custom", "tool");
        preset.env.insert("CARGO_TERM_COLOR".to_owned(), "never".to_owned());

        let mut spec = StageSpec {
            hooks: vec![plain, preset],
            ..StageSpec::default()
        };
        force_child_color(&mut spec);

        // The plain hook gains all three force-colour vars.
        assert_eq!(
            spec.hooks[0].env.get("CARGO_TERM_COLOR").map(String::as_str),
            Some("always")
        );
        assert_eq!(spec.hooks[0].env.get("CLICOLOR_FORCE").map(String::as_str), Some("1"));
        assert_eq!(spec.hooks[0].env.get("FORCE_COLOR").map(String::as_str), Some("1"));
        // The user's explicit value wins; the other vars are still added.
        assert_eq!(
            spec.hooks[1].env.get("CARGO_TERM_COLOR").map(String::as_str),
            Some("never")
        );
        assert_eq!(spec.hooks[1].env.get("CLICOLOR_FORCE").map(String::as_str), Some("1"));
    }

    #[test]
    fn retain_keeps_workspace_hooks_forces_always_run_and_drops_steps() {
        use poly_hooks::{Hook, StageSpec};

        let mut ws = Hook::run("go-vet", "go vet ./...");
        ws.workspace = true;
        ws.always_run = false;
        let per_file = Hook::run("fmt", "poly fmt");
        let mut opted_out = Hook::run("cargo-clippy", "cargo clippy");
        opted_out.workspace = true;
        opted_out.skip_in_lint = true;

        let mut spec = StageSpec {
            precondition: Some("test -f Cargo.toml".to_string()),
            before: vec!["echo setup".to_string()],
            after: vec!["echo teardown".to_string()],
            hooks: vec![ws, per_file, opted_out],
            ..StageSpec::default()
        };
        retain_workspace_hooks(&mut spec);

        assert_eq!(spec.hooks.len(), 1, "only the non-opted-out workspace hook is kept");
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

        let wrong_type: toml::Table = toml::from_str("workspace = \"no\"").unwrap();
        assert!(!workspace_lint_disabled(&wrong_type));
    }
}
