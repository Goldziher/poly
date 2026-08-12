//! Subprocess construction and execution for the hook runner.
//!
//! Everything that turns a [`Hook`] (or a bare `before`/`after`/`precondition`
//! command line) into a running process lives here: shell selection and
//! quoting, `CARGO_TARGET_DIR` / sccache environment injection, and the
//! capture-or-preview output plumbing.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Once;
use std::time::{Duration, Instant};

use indicatif::ProgressBar;
use tracing::warn;

use crate::model::{Hook, HookCommand, HookStatus, SccacheSettings, StepOutcome, TimeoutReason};
use crate::process::{Cmd, Error as ProcessError, OutputSink};
use crate::reporter::{CaptureSink, PreviewSink};
use crate::supervise::Supervised;
use crate::timeout::Budget;

#[cfg(not(windows))]
const SHELL: &str = "sh";
#[cfg(not(windows))]
const SHELL_ARG: &str = "-c";
#[cfg(windows)]
const SHELL: &str = "cmd";
#[cfg(windows)]
const SHELL_ARG: &str = "/C";

pub(super) fn build_command(
    hook: &Hook,
    root: &Path,
    files: &[&Path],
    sccache: Option<&SccacheSettings>,
    cargo_target_dir: Option<&Path>,
) -> Cmd {
    let mut cmd = match &hook.command {
        HookCommand::Run(line) => shell_command(line, &hook.args, files, hook.pass_filenames),
        HookCommand::Script { path, runner } => {
            let mut cmd = match runner {
                Some(runner) => {
                    let mut cmd = Cmd::new(runner, hook.id.clone());
                    cmd.arg(path);
                    cmd
                }
                None => Cmd::new(path, hook.id.clone()),
            };
            cmd.args(&hook.args);
            if hook.pass_filenames {
                cmd.args(files.iter().map(|p| p.as_os_str()));
            }
            cmd
        }
    };
    cmd.current_dir(hook_working_dir(hook, root));
    cmd.envs(hook.env.iter());
    if let Some(target) = cargo_target_dir {
        if !hook.env.contains_key("CARGO_TARGET_DIR") {
            cmd.env("CARGO_TARGET_DIR", target);
        }
    }
    inject_sccache_env(&mut cmd, hook, sccache);
    cmd
}

/// The directory a hook (and therefore its own `precondition`/`before`) runs
/// from: the execution root, plus the hook's `cwd` override when set.
pub(super) fn hook_working_dir(hook: &Hook, root: &Path) -> std::path::PathBuf {
    hook.cwd
        .as_deref()
        .map_or_else(|| root.to_path_buf(), |relative| root.join(relative))
}

/// Module-global guard so the shared sccache server is started at most once per
/// `poly hooks` process, no matter how many compiler hooks or batches run.
static SCCACHE_SERVER_START: Once = Once::new();

/// Inject the tier-2 sccache environment into a compiler hook's command.
///
/// A no-op unless the hook opted in via [`Hook::compiler`] **and** the run
/// carries [`SccacheSettings`]. Starts the shared sccache server once per
/// process (best-effort — a start failure only warns, since sccache also
/// auto-starts on first client use), then sets `RUSTC_WRAPPER` plus the
/// optional `SCCACHE_DIR` / `SCCACHE_CACHE_SIZE`.
///
/// Caveat: if an sccache server is already running with a different `SCCACHE_DIR`
/// / size, the client env is ignored by that server — this is accepted.
fn inject_sccache_env(cmd: &mut Cmd, hook: &Hook, sccache: Option<&SccacheSettings>) {
    if !hook.compiler {
        return;
    }
    let Some(settings) = sccache else {
        return;
    };
    ensure_sccache_server(settings);
    cmd.env("RUSTC_WRAPPER", &settings.bin);
    if let Some(dir) = &settings.dir {
        cmd.env("SCCACHE_DIR", dir);
    }
    if let Some(max_size) = &settings.max_size {
        cmd.env("SCCACHE_CACHE_SIZE", max_size);
    }
}

/// Start the sccache server idempotently (once per process), with the resolved
/// `SCCACHE_DIR` / `SCCACHE_CACHE_SIZE` in its own environment. Best-effort: a
/// launch failure is logged and ignored.
fn ensure_sccache_server(settings: &SccacheSettings) {
    SCCACHE_SERVER_START.call_once(|| {
        let mut cmd = Cmd::new(&settings.bin, format!("{} --start-server", settings.bin));
        cmd.arg("--start-server");
        if let Some(dir) = &settings.dir {
            cmd.env("SCCACHE_DIR", dir);
        }
        if let Some(max_size) = &settings.max_size {
            cmd.env("SCCACHE_CACHE_SIZE", max_size);
        }
        cmd.check(false).stdout(Stdio::null()).stderr(Stdio::null());
        if let Err(error) = cmd.status() {
            warn!("failed to start sccache server: {error}");
        }
    });
}

#[cfg(not(windows))]
fn shell_command(line: &str, args: &[String], files: &[&Path], pass_filenames: bool) -> Cmd {
    let mut cmd = Cmd::new(SHELL, line.to_string());
    cmd.arg(SHELL_ARG).arg(format!("{line} \"$@\"")).arg("poly-hook");
    cmd.args(args);
    if pass_filenames {
        cmd.args(files.iter().map(|p| p.as_os_str()));
    }
    cmd
}

/// Quote a token for inclusion in a `cmd /C` command line so an
/// attacker-controlled value (notably a tracked filename like `foo & evil.exe`)
/// cannot inject cmd.exe syntax. Wrap in double quotes — which neutralizes the
/// metacharacters cmd interprets outside quotes (`&`, `|`, `<`, `>`, `(`, `)`,
/// whitespace) — doubling any embedded `"` and escaping `%`.
///
/// Kept un-gated so the quoting logic is unit-tested on every platform; it is
/// only *called* from the `cfg(windows)` `shell_command` below.
#[cfg_attr(not(windows), allow(dead_code))]
fn cmd_quote(value: &str) -> String {
    let escaped = value.replace('"', "\"\"").replace('%', "%%");
    format!("\"{escaped}\"")
}

#[cfg(windows)]
fn shell_command(line: &str, args: &[String], files: &[&Path], pass_filenames: bool) -> Cmd {
    let mut joined = line.to_string();
    for arg in args {
        joined.push(' ');
        joined.push_str(&cmd_quote(arg));
    }
    if pass_filenames {
        for file in files {
            joined.push(' ');
            joined.push_str(&cmd_quote(&file.to_string_lossy()));
        }
    }
    let mut cmd = Cmd::new(SHELL, line.to_string());
    cmd.arg(SHELL_ARG).arg(joined);
    cmd
}

/// Run one command to completion — or to the end of its `budget` — capturing
/// its combined output.
///
/// When `bar` is present the output is streamed live into the hook's spinner
/// via a [`PreviewSink`]; otherwise a plain [`CaptureSink`] just accumulates
/// it. While the command runs, the budget's announce cadence prints a
/// still-running notice naming `id`, so a hang is attributable while it is
/// happening rather than only in hindsight.
pub(super) fn execute(mut cmd: Cmd, bar: Option<&ProgressBar>, id: &str, budget: Budget) -> (HookStatus, Vec<u8>) {
    cmd.check(false);
    let notify = |elapsed: Duration| announce_still_running(bar, id, elapsed, budget.limit);
    let (result, bytes) = if let Some(bar) = bar {
        let mut sink = PreviewSink::new(bar, id);
        let result = capture(&mut cmd, &mut sink, budget, &notify);
        (result, sink.into_bytes())
    } else {
        let mut sink = CaptureSink::default();
        let result = capture(&mut cmd, &mut sink, budget, &notify);
        (result, sink.into_bytes())
    };
    match result {
        Ok(run) => (status_of(&run, budget), bytes),
        Err(error) => (HookStatus::Error(error.to_string()), bytes),
    }
}

/// Run `cmd` under `budget`, falling back to the plain unbounded capture when
/// the budget neither kills nor announces — so disabling timeouts restores the
/// previous execution path exactly, threads and all.
fn capture<S: OutputSink>(
    cmd: &mut Cmd,
    sink: S,
    budget: Budget,
    notify: &dyn Fn(Duration),
) -> Result<Supervised, ProcessError> {
    if budget.is_supervised() {
        return cmd.output_with_sink_supervised(sink, budget, notify);
    }
    let started = Instant::now();
    cmd.output_with_sink(sink)
        .map(|output| Supervised::completed(output, started.elapsed()))
}

/// Classify a completed run. A killed process is reported as
/// [`HookStatus::TimedOut`] and never as a plain failure: its exit status
/// describes poly's signal, not the tool's opinion of the code.
fn status_of(run: &Supervised, budget: Budget) -> HookStatus {
    if run.timed_out {
        return HookStatus::TimedOut(TimeoutReason {
            limit: budget.limit.unwrap_or(run.elapsed),
            elapsed: run.elapsed,
        });
    }
    if run.output.status.success() {
        HookStatus::Passed
    } else {
        HookStatus::Failed {
            code: run.output.status.code(),
        }
    }
}

/// Put the running hook's id on screen while it is still running.
///
/// Routed through the progress bar when there is one (so it lands above the
/// live spinners instead of tearing them), and straight to stderr otherwise —
/// which is the case that matters, because a non-interactive run has no spinner
/// to reveal what is hanging.
fn announce_still_running(bar: Option<&ProgressBar>, id: &str, elapsed: Duration, limit: Option<Duration>) {
    let line = crate::reporter::still_running_line(id, elapsed, limit);
    // A hidden bar swallows `println`, which is precisely the case that must not
    // stay silent: progress requested, but nothing is drawing it.
    match bar.filter(|bar| !bar.is_hidden()) {
        Some(bar) => bar.println(line),
        None => eprintln!("{line}"),
    }
}

/// Run a `before`/`after` shell command from `root`, capturing its output.
///
/// `env` is layered over the inherited environment — empty for a stage-level
/// step, the hook's own declared `env` for a per-hook one, so a hook's setup
/// sees exactly what the hook will.
pub(super) fn run_step(root: &Path, command: &str, env: &BTreeMap<String, String>) -> StepOutcome {
    let mut cmd = Cmd::new(SHELL, command.to_string());
    cmd.arg(SHELL_ARG).arg(command).current_dir(root).envs(env.iter());
    let (status, output) = execute(cmd, None, command, Budget::unlimited());
    StepOutcome {
        command: command.to_string(),
        status,
        output,
    }
}

/// Evaluate a `precondition` guard from `root`: `true` when it exits 0.
///
/// Output is discarded — a precondition is a probe, not a check, so its chatter
/// never reaches the report.
pub(super) fn run_precondition(root: &Path, command: &str, env: &BTreeMap<String, String>) -> bool {
    let mut cmd = Cmd::new(SHELL, command.to_string());
    cmd.arg(SHELL_ARG)
        .arg(command)
        .current_dir(root)
        .envs(env.iter())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .check(false);
    cmd.status().is_ok_and(|status| status.success())
}

/// Estimate the argv bytes consumed by everything except the matched files, so
/// `ARG_MAX` batching reserves the right headroom.
pub(super) fn base_arg_len(hook: &Hook) -> usize {
    const FIXED: usize = 256;
    let command_len = match &hook.command {
        HookCommand::Run(line) => line.len(),
        HookCommand::Script { path, runner } => path.len() + runner.as_ref().map_or(0, String::len),
    };
    let args_len: usize = hook.args.iter().map(|a| a.len() + 9).sum();
    FIXED + command_len + args_len
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use super::{Hook, SccacheSettings, build_command, cmd_quote};

    #[test]
    fn cmd_quote_neutralizes_metacharacters() {
        assert_eq!(cmd_quote("foo.rs & evil.exe"), "\"foo.rs & evil.exe\"");
        assert_eq!(cmd_quote("a\"b"), "\"a\"\"b\"");
        assert_eq!(cmd_quote("100%done"), "\"100%%done\"");
    }

    /// Collect the explicit environment overrides a built [`super::Cmd`] carries.
    fn injected_env(hook: &Hook, sccache: Option<&SccacheSettings>) -> HashMap<String, String> {
        let cmd = build_command(hook, Path::new("."), &[], sccache, None);
        cmd.get_envs()
            .filter_map(|(key, value)| {
                value.map(|value| (key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()))
            })
            .collect()
    }

    /// `bin = "true"` keeps the one-shot `--start-server` probe harmless: `true`
    /// ignores its arguments and exits 0, so the test never requires sccache.
    fn settings() -> SccacheSettings {
        SccacheSettings {
            bin: "true".to_string(),
            dir: Some(std::path::PathBuf::from("/tmp/sccache-test")),
            max_size: Some("2G".to_string()),
        }
    }

    #[test]
    fn compiler_hook_gets_sccache_env_injected() {
        let mut hook = Hook::run("clippy", "cargo clippy");
        hook.compiler = true;
        let env = injected_env(&hook, Some(&settings()));
        assert_eq!(env.get("RUSTC_WRAPPER").map(String::as_str), Some("true"));
        assert_eq!(env.get("SCCACHE_DIR").map(String::as_str), Some("/tmp/sccache-test"));
        assert_eq!(env.get("SCCACHE_CACHE_SIZE").map(String::as_str), Some("2G"));
    }

    #[test]
    fn non_compiler_hook_gets_no_sccache_env() {
        let hook = Hook::run("fmt", "cargo fmt --check");
        let env = injected_env(&hook, Some(&settings()));
        assert!(!env.contains_key("RUSTC_WRAPPER"), "env: {env:?}");
        assert!(!env.contains_key("SCCACHE_DIR"), "env: {env:?}");
    }

    #[test]
    fn compiler_hook_without_settings_gets_no_sccache_env() {
        let mut hook = Hook::run("clippy", "cargo clippy");
        hook.compiler = true;
        let env = injected_env(&hook, None);
        assert!(!env.contains_key("RUSTC_WRAPPER"), "env: {env:?}");
    }

    #[test]
    fn sccache_settings_without_dir_omits_dir_env() {
        let mut hook = Hook::run("clippy", "cargo clippy");
        hook.compiler = true;
        let bare = SccacheSettings {
            bin: "true".to_string(),
            dir: None,
            max_size: None,
        };
        let env = injected_env(&hook, Some(&bare));
        assert_eq!(env.get("RUSTC_WRAPPER").map(String::as_str), Some("true"));
        assert!(!env.contains_key("SCCACHE_DIR"), "env: {env:?}");
        assert!(!env.contains_key("SCCACHE_CACHE_SIZE"), "env: {env:?}");
    }
}
