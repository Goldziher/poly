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
    if spec.hooks.is_empty() {
        return Ok(true);
    }

    // When poly's own output is coloured, make the captured tools emit colour too
    // (they otherwise see a capture pipe, not a TTY, and self-disable it). Gated on
    // the same decision as the report below, so `--no-color`/redirected output stays
    // clean. Paired with the pass-through in `append_output`.
    let color = color_enabled(args.to_stdout);
    if color {
        force_child_color(&mut spec);
    }

    let cache = open_result_cache(&config, &root, args.no_cache)?;
    let sccache = sccache_settings(&config, false)?;
    let request = poly_hooks::HookRunRequest {
        root,
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
    render(&outcome, args.to_stdout, color);
    Ok(outcome.success())
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
/// - drop the stage's `precondition` / `before` / `after` scaffolding: `poly lint`
///   runs the tools, not the user's commit-time setup/teardown (a failing `before`
///   would otherwise abort with no rendered explanation).
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
/// beneath it. Written to stdout for pretty output, else stderr.
fn render(outcome: &poly_hooks::HookRunOutcome, to_stdout: bool, color: bool) {
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
                append_output(&mut buffer, &hook.output, color);
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
