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

use crate::cargo_lock::{self, PACKAGE_CACHE_RESOURCE, Wait, WaitPlan};
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
        HookCommand::Run(line) => shell_command(line, &hook.id, &hook.args, files, hook.pass_filenames),
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

/// `$0` for a `run` line whose hook has no id, so a shell diagnostic still says
/// something rather than opening with a bare colon. Unix-only: `cmd /C` has no
/// `$0` to set.
#[cfg(not(windows))]
const ANONYMOUS_HOOK: &str = "poly-hook";

/// Reserved words that close a compound command (`... done`, `... fi`,
/// `... esac`, `{ ...; }`). No plain word may follow one, so appending `"$@"`
/// after it is a hard parse error — the `syntax error near unexpected token
/// "$@"` of #46.
const COMPOUND_TERMINATORS: &[&str] = &["done", "fi", "esac", "}"];

/// Reserved words that leave a compound command *unfinished*. A line ending in
/// one is already a parse error on its own; the append only relocates the
/// shell's complaint onto poly's text.
const COMPOUND_CONTINUATIONS: &[&str] = &["then", "else", "elif", "do", "in", "{"];

/// Whether a trailing word may be appended to `line` without changing how the
/// shell parses it.
///
/// This is deliberately a *tail* test rather than a shell parser: the goal is to
/// recognise the shapes that cannot take an appended argument, not to validate
/// the line. Everything it does not recognise keeps the historical behaviour.
fn takes_trailing_word(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    // A separator, an operator or a background `&` ends the command: an appended
    // word would start a *new* one, so the first filename would be executed
    // rather than passed. A dangling redirect operator is worse still — the
    // append would become its target and truncate a source file.
    if line.ends_with([';', '&', '|', '>', '<']) {
        return false;
    }
    // A whole-line subshell closes with `)`, which no word may follow. Matched
    // on the pair so a command substitution (`ls $(pwd)`) is untouched.
    if line.starts_with('(') && line.ends_with(')') {
        return false;
    }
    let tail = line.rsplit(char::is_whitespace).next().unwrap_or_default();
    !COMPOUND_TERMINATORS.contains(&tail) && !COMPOUND_CONTINUATIONS.contains(&tail)
}

/// Whether `line` already forwards the full positional list itself.
///
/// `$1` is deliberately *not* treated as self-management: `grep -q TODO "$1" &&
/// shellcheck` guards on the first file while still relying on the append to
/// pass every file, and that line works today. Only `$@` / `$*` are unambiguous
/// — a line that already expands all of them would simply see every argument
/// twice if poly appended them again.
fn forwards_positionals(line: &str) -> bool {
    line.contains("$@") || line.contains("$*") || line.contains("${@") || line.contains("${*")
}

/// Whether poly may append its `"$@"` (or, on Windows, the quoted argument list)
/// to a `run` line.
///
/// poly always *supplies* the hook's `args` and matched files as the shell's
/// positional parameters; the append is the sugar that forwards them on to a
/// bare command, so `run = "shellcheck"` behaves like the pre-commit convention
/// consumers expect. A line that is a script rather than a simple command reads
/// `"$@"` itself, which is the ordinary `sh` contract and is why the append can
/// be dropped without losing the files.
///
/// Shared by both platform implementations of [`shell_command`] so a given
/// `run` line gets poly's appended arguments either everywhere or nowhere.
/// Caveat: `cmd.exe` has no positional parameters, so on Windows a line that
/// opts out of the append is genuinely on its own for its file list.
fn appends_positionals(line: &str) -> bool {
    !forwards_positionals(line) && takes_trailing_word(line)
}

#[cfg(not(windows))]
fn shell_command(line: &str, id: &str, args: &[String], files: &[&Path], pass_filenames: bool) -> Cmd {
    let mut cmd = Cmd::new(SHELL, line.to_string());
    let script = if appends_positionals(line) {
        format!("{line} \"$@\"")
    } else {
        line.to_string()
    };
    // `$0` names the hook so a shell diagnostic about the line — a syntax error,
    // a missing command — is attributable to the hook that configured it instead
    // of to poly.
    let argv0 = if id.is_empty() { ANONYMOUS_HOOK } else { id };
    cmd.arg(SHELL_ARG).arg(script).arg(argv0);
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
/// Backslashes need care for the opposite reason: `CommandLineToArgvW` (and the
/// C runtime's argv parser) treat a run of backslashes as an escape *only when a
/// `"` follows it*. A value ending in an odd number of backslashes — a hook
/// `args` entry or a directory such as `C:\build\` — would therefore escape the
/// closing quote, so the quoted region never terminates and whatever follows is
/// reparsed as command-line syntax. Doubling only the run that precedes a `"`
/// and the run that precedes the closing quote fixes that while leaving an
/// ordinary path like `C:\src\main.rs` byte-for-byte unchanged.
///
/// This is string-level defence: it is unit-tested as a pure function and never
/// handed to a real `cmd.exe`, so it proves the shape of the output, not that
/// cmd.exe parses it safely.
///
/// Kept un-gated so the quoting logic is unit-tested on every platform; it is
/// only *called* from the `cfg(windows)` `shell_command` below.
#[cfg_attr(not(windows), allow(dead_code))]
fn cmd_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    let mut backslashes = 0usize;
    for character in value.chars() {
        match character {
            '\\' => {
                backslashes += 1;
                quoted.push('\\');
            }
            '"' => {
                // The run is about to precede a quote, so it becomes an escape
                // unless every backslash is itself escaped first.
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                quoted.push_str("\"\"");
            }
            '%' => {
                backslashes = 0;
                quoted.push_str("%%");
            }
            _ => {
                backslashes = 0;
                quoted.push(character);
            }
        }
    }
    // Same rule for the run that runs up against the closing quote.
    quoted.extend(std::iter::repeat_n('\\', backslashes));
    quoted.push('"');
    quoted
}

#[cfg(windows)]
// `_id`: cmd.exe has no `$0`, so the hook id cannot travel with the command
// line the way it does on Unix; it still reaches the report via the runner.
fn shell_command(line: &str, _id: &str, args: &[String], files: &[&Path], pass_filenames: bool) -> Cmd {
    let mut joined = line.to_string();
    if appends_positionals(line) {
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
    let notify = |elapsed: Duration, waiting_on: Option<&str>| {
        announce_still_running(bar, id, elapsed, budget.limit, waiting_on);
    };
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
    notify: &dyn Fn(Duration, Option<&str>),
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
///
/// Reading the kill flag ahead of the status is only sound because
/// [`Supervised::timed_out`] means *the kill is what ended the child*, not
/// merely that poly went to kill it — a child that finished on its own at the
/// deadline arrives here with the flag clear and is classified on its own exit
/// code, which is the difference between reporting a passing hook and blocking
/// a commit on it.
fn status_of(run: &Supervised, budget: Budget) -> HookStatus {
    if run.timed_out {
        return HookStatus::TimedOut(TimeoutReason::command(budget.limit.unwrap_or(run.elapsed), run.elapsed));
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
///
/// `waiting_on` names the lock the hook is queued behind, when it said so:
/// "quiet because it is working" and "quiet because cargo will not let it start"
/// are different diagnoses and the notice reports whichever one is true.
fn announce_still_running(
    bar: Option<&ProgressBar>,
    id: &str,
    elapsed: Duration,
    limit: Option<Duration>,
    waiting_on: Option<&str>,
) {
    announce(bar, crate::reporter::still_running_line(id, elapsed, limit, waiting_on));
}

/// Put `line` on screen above the live spinners, or on stderr when nothing is
/// drawing them.
///
/// A hidden bar swallows `println`, which is precisely the case that must not
/// stay silent: progress requested, but nothing is drawing it.
fn announce(bar: Option<&ProgressBar>, line: String) {
    match bar.filter(|bar| !bar.is_hidden()) {
        Some(bar) => bar.println(line),
        None => eprintln!("{line}"),
    }
}

/// Hold `id` back until cargo's package-cache lock is free, so a holder outside
/// this run is not charged against the hook's budget.
///
/// Called only for a hook in the cargo exclusion set ([`Hook::is_cargo`]), and
/// only from the runner, which knows what the hook is; the supervisor sees an
/// argv it cannot classify and must not try.
///
/// This is a mitigation and not a fix — the lock can be taken between the probe
/// and the spawn, and cargo re-takes it mid-run — so the hook is spawned when the
/// bound expires rather than being withheld: a check that did not run must never
/// be reported as a pass, and running late beats failing a commit for somebody
/// else's `cargo build`. From that point the post-spawn notice, driven by the
/// child's own `Blocking waiting for file lock` line, describes the rest.
pub(super) fn await_cargo_package_cache(id: &str, budget: Budget, bar: Option<&ProgressBar>) {
    let (Some(path), Some(plan)) = (cargo_lock::package_cache_lock(), WaitPlan::for_budget(budget)) else {
        return;
    };
    let waited = cargo_lock::wait_until_free(&path, plan, &|waited| {
        announce(
            bar,
            crate::reporter::lock_wait_line(id, waited, plan.bound, PACKAGE_CACHE_RESOURCE),
        );
    });
    if let Wait::Expired(waited) = waited {
        warn!(
            hook = id,
            "cargo's {PACKAGE_CACHE_RESOURCE} lock was still held after {waited:.1?}; starting the hook anyway — \
             its budget now includes whatever is left of that wait"
        );
    }
}

/// Run a `before`/`after` shell command from `root` under `budget`, capturing
/// its output.
///
/// `env` is layered over the inherited environment — empty for a stage-level
/// step, the hook's own declared `env` for a per-hook one, so a hook's setup
/// sees exactly what the hook will.
///
/// A step that overruns is killed exactly as a hook is, and reports
/// [`HookStatus::TimedOut`]: a setup step that hangs blocks the commit just as
/// completely as a hook that hangs, and the caller has to be able to tell that
/// apart from a step that failed on its own merits.
pub(super) fn run_step(root: &Path, command: &str, env: &BTreeMap<String, String>, budget: Budget) -> StepOutcome {
    let mut cmd = Cmd::new(SHELL, command.to_string());
    cmd.arg(SHELL_ARG).arg(command).current_dir(root).envs(env.iter());
    let (status, output) = execute(cmd, None, command, budget);
    StepOutcome {
        command: command.to_string(),
        status,
        output,
    }
}

/// What a `precondition` probe answered.
///
/// [`Probe::TimedOut`] exists because the alternative is a false pass: reading
/// a wedged probe as "does not apply" would withhold every hook it guards and
/// let the run report success having validated nothing.
pub(super) enum Probe {
    /// Exit 0 — the guarded hooks apply.
    Passed,
    /// Non-zero, or the probe could not be launched — they do not apply.
    Declined,
    /// poly killed the probe; whether they apply is unknown.
    TimedOut(TimeoutReason),
}

/// Evaluate a `precondition` guard from `root` under `budget`.
///
/// Output is discarded — a precondition is a probe, not a check, so its chatter
/// never reaches the report.
pub(super) fn run_precondition(root: &Path, command: &str, env: &BTreeMap<String, String>, budget: Budget) -> Probe {
    let mut cmd = Cmd::new(SHELL, command.to_string());
    cmd.arg(SHELL_ARG).arg(command).current_dir(root).envs(env.iter());

    if !budget.is_supervised() {
        // The un-supervised path is kept byte for byte: no pipes, no drain
        // threads, output straight to /dev/null.
        cmd.stdout(Stdio::null()).stderr(Stdio::null()).check(false);
        return match cmd.status() {
            Ok(status) if status.success() => Probe::Passed,
            _ => Probe::Declined,
        };
    }

    // Supervision needs pipes, so the output is captured and dropped rather
    // than never produced — the same nothing, from the report's point of view.
    match execute(cmd, None, command, budget).0 {
        HookStatus::Passed => Probe::Passed,
        HookStatus::TimedOut(reason) => Probe::TimedOut(TimeoutReason::precondition(reason.limit, reason.elapsed)),
        _ => Probe::Declined,
    }
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

    use super::{Hook, SccacheSettings, appends_positionals, build_command, cmd_quote};

    /// The argv poly hands the shell, as lossy strings. On Unix that is
    /// `[-c, script, $0, args…, files…]`.
    fn shell_argv(hook: &Hook, files: &[&Path]) -> Vec<String> {
        build_command(hook, Path::new("."), files, None, None)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// The `run` line as the shell actually receives it — poly's append included
    /// when it made one.
    fn shell_script(hook: &Hook, files: &[&Path]) -> String {
        shell_argv(hook, files).get(1).cloned().unwrap_or_default()
    }

    /// A simple command cannot forward the matched files by itself, so poly
    /// appends `"$@"` — this is the pre-commit convention (`run = "shellcheck"`
    /// lints the matched files) and the reason the append cannot just be dropped.
    #[test]
    fn simple_command_line_gets_the_positional_forward_appended() {
        let cases = [
            ("shellcheck", "shellcheck \"$@\""),
            ("cargo fmt --check", "cargo fmt --check \"$@\""),
            // A complete redirect still ends in a word position.
            ("tool 2>&1", "tool 2>&1 \"$@\""),
            ("tool >&2", "tool >&2 \"$@\""),
            ("tool > out.log", "tool > out.log \"$@\""),
            // A `${…}` / `$(…)` tail is an ordinary word, not a closing brace.
            (
                "fmt --manifest-path ${DIR}/Cargo.toml",
                "fmt --manifest-path ${DIR}/Cargo.toml \"$@\"",
            ),
            ("fmt --manifest-path $(pwd)", "fmt --manifest-path $(pwd) \"$@\""),
            // `$1` alone is a guard, not self-management: this line reads the
            // first file *and* relies on the append for the rest.
            (
                "grep -q TODO \"$1\" && shellcheck",
                "grep -q TODO \"$1\" && shellcheck \"$@\"",
            ),
        ];
        for (line, expected) in cases {
            let hook = Hook::run("hook-id", line);
            assert_eq!(shell_script(&hook, &[Path::new("a.sh")]), expected, "line: {line:?}");
        }
    }

    /// #46: a `run` line that is a script rather than a simple command cannot
    /// take a trailing word, so poly leaves it verbatim. The files are still
    /// supplied as positional parameters — the line reads `"$@"` itself, which is
    /// the ordinary `sh` contract.
    #[test]
    fn script_shaped_run_line_is_passed_to_the_shell_verbatim() {
        let lines = [
            "for f in \"$@\"; do shellcheck \"$f\"; done",
            "if [ -f Cargo.toml ]; then cargo fmt --check; fi",
            "case $MODE in ci) cargo test;; esac",
            "{ cargo fmt --check; }",
            "(cd sub && cargo test)",
            "while read -r line; do echo \"$line\"; done",
        ];
        for line in lines {
            let hook = Hook::run("hook-id", line);
            assert_eq!(shell_script(&hook, &[Path::new("a.sh")]), line, "line: {line:?}");
        }
    }

    /// A line that already expands the whole positional list would see every
    /// argument twice if poly appended it again.
    #[test]
    fn line_that_forwards_positionals_itself_is_not_appended_to() {
        let lines = ["printf '%s\\n' \"$@\"", "shellcheck -- $*", "tool ${@}", "tool ${*}"];
        for line in lines {
            let hook = Hook::run("hook-id", line);
            assert_eq!(shell_script(&hook, &[Path::new("a.sh")]), line, "line: {line:?}");
        }
    }

    /// A separator or operator tail ends the command: an appended `"$@"` would
    /// start a *new* one and the first filename would be executed. A dangling
    /// redirect is worse — the append would become its target and truncate the
    /// file poly meant to lint.
    #[test]
    fn separator_and_dangling_redirect_tails_are_not_appended_to() {
        let lines = [
            "cargo fmt --check;",
            "sleep 1 &",
            "cargo build &&",
            "grep -r TODO |",
            "cargo build >",
            "tool 2>",
        ];
        for line in lines {
            let hook = Hook::run("hook-id", line);
            assert_eq!(shell_script(&hook, &[Path::new("a.sh")]), line, "line: {line:?}");
        }
    }

    /// The append decision is a pure function of the line, shared by the Unix and
    /// Windows implementations of `shell_command`, so a `run` line gets poly's
    /// appended arguments either on every platform or on none. This table is what
    /// pins that agreement, and it runs everywhere.
    #[test]
    fn appends_positionals_is_decided_by_the_line_alone() {
        let cases = [
            ("shellcheck", true),
            ("cargo fmt --check", true),
            ("tool 2>&1", true),
            ("tool ${DIR}", true),
            ("for f in \"$@\"; do :; done", false),
            ("if true; then cargo test; fi", false),
            ("case x in x) :;; esac", false),
            ("{ cargo test; }", false),
            ("(cd sub && cargo test)", false),
            ("printf '%s' \"$@\"", false),
            ("tool $*", false),
            ("cargo test;", false),
            ("cargo test &", false),
            ("cargo test |", false),
            ("cargo test >", false),
            ("", false),
        ];
        for (line, expected) in cases {
            assert_eq!(appends_positionals(line), expected, "line: {line:?}");
        }
    }

    /// `$0` is the hook id so a shell diagnostic about the line — a syntax error,
    /// a missing command — names the hook that configured it rather than reading
    /// as a poly bug.
    #[test]
    #[cfg(not(windows))]
    fn shell_argv_zero_is_the_hook_id_so_diagnostics_name_the_hook() {
        let hook = Hook::run("rust-max-lines", "shellcheck");
        assert_eq!(
            shell_argv(&hook, &[Path::new("a.sh")]),
            vec!["-c", "shellcheck \"$@\"", "rust-max-lines", "a.sh"]
        );
    }

    /// An id-less hook still gets a usable `$0`: a bare colon would open the
    /// shell's diagnostic with no subject at all.
    #[test]
    #[cfg(not(windows))]
    fn id_less_hook_falls_back_to_a_named_argv_zero() {
        let mut hook = Hook::run("", "shellcheck");
        hook.pass_filenames = false;
        assert_eq!(shell_argv(&hook, &[]), vec!["-c", "shellcheck \"$@\"", "poly-hook"]);
    }

    /// Skipping the append never withholds the files: they stay in argv as the
    /// script's positional parameters, after `$0` and the hook's own `args`.
    #[test]
    #[cfg(not(windows))]
    fn a_script_shaped_line_still_receives_args_and_files_as_positionals() {
        let mut hook = Hook::run("loop", "for f in \"$@\"; do shellcheck \"$f\"; done");
        hook.args = vec!["--severity=error".to_string()];
        assert_eq!(
            shell_argv(&hook, &[Path::new("a.sh"), Path::new("b.sh")]),
            vec![
                "-c",
                "for f in \"$@\"; do shellcheck \"$f\"; done",
                "loop",
                "--severity=error",
                "a.sh",
                "b.sh"
            ]
        );
    }

    /// Effect, not shape: #46 is a *runtime* failure, so this runs the command
    /// poly builds and reads what the shell did with it.
    #[test]
    #[cfg(unix)]
    fn script_shaped_run_line_executes_and_sees_every_file() {
        let hook = Hook::run("loop", "for f in \"$@\"; do printf 'got:%s\\n' \"$f\"; done");
        let mut cmd = build_command(
            &hook,
            Path::new("."),
            &[Path::new("a.rs"), Path::new("b.rs")],
            None,
            None,
        );
        cmd.check(false);
        let output = cmd.output().expect("the built command must launch");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "got:a.rs\ngot:b.rs\n");
        assert_eq!(output.status.code(), Some(0));
    }

    /// The companion effect test for the common case: the append is what puts the
    /// matched files on a bare command's argv.
    #[test]
    #[cfg(unix)]
    fn simple_run_line_executes_with_the_matched_files_appended() {
        let hook = Hook::run("printer", "printf '[%s]'");
        let mut cmd = build_command(
            &hook,
            Path::new("."),
            &[Path::new("a.rs"), Path::new("b.rs")],
            None,
            None,
        );
        cmd.check(false);
        let output = cmd.output().expect("the built command must launch");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "[a.rs][b.rs]");
        assert_eq!(output.status.code(), Some(0));
    }

    /// A shell diagnostic really is prefixed with the hook id, end to end.
    #[test]
    #[cfg(unix)]
    fn shell_diagnostic_is_prefixed_with_the_hook_id() {
        let hook = Hook::run("no-such-tool", "definitely-not-a-real-command-9f3a");
        let mut cmd = build_command(&hook, Path::new("."), &[], None, None);
        cmd.check(false).stderr(std::process::Stdio::piped());
        let output = cmd.output().expect("the built command must launch");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("no-such-tool:"), "stderr: {stderr:?}");
        assert_eq!(output.status.code(), Some(127));
    }

    /// `cmd.exe` interprets `&`, `|`, `<`, `>`, `(`, `)`, `^`, and whitespace as
    /// syntax only *outside* a double-quoted region. `cmd_quote` neutralizes all
    /// of them by wrapping in quotes, without needing to escape any of them
    /// individually — so each one must survive the round trip unmodified,
    /// sitting inside the pair of quotes `cmd_quote` adds.
    #[test]
    fn cmd_quote_wraps_metacharacters_dangerous_outside_quotes() {
        let cases = [
            ("foo&bar", "\"foo&bar\""),
            ("foo|bar", "\"foo|bar\""),
            ("foo<bar", "\"foo<bar\""),
            ("foo>bar", "\"foo>bar\""),
            ("foo(bar)", "\"foo(bar)\""),
            ("foo^bar", "\"foo^bar\""),
            ("foo bar", "\"foo bar\""),
        ];
        for (input, expected) in cases {
            assert_eq!(cmd_quote(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn cmd_quote_neutralizes_ampersand_command_chaining() {
        assert_eq!(cmd_quote("foo.rs & evil.exe"), "\"foo.rs & evil.exe\"");
        assert_eq!(cmd_quote("foo & calc.exe"), "\"foo & calc.exe\"");
    }

    /// The trailing `C:\` is the trailing-backslash hazard as well as a chaining
    /// attempt: the backslash is doubled so it cannot escape the closing quote,
    /// which is what keeps the `&&` sealed inside the quoted region.
    #[test]
    fn cmd_quote_neutralizes_double_ampersand_chaining() {
        assert_eq!(cmd_quote("foo && del /f /q C:\\"), "\"foo && del /f /q C:\\\\\"");
    }

    #[test]
    fn cmd_quote_neutralizes_pipe_to_more() {
        assert_eq!(cmd_quote("foo | more"), "\"foo | more\"");
    }

    #[test]
    fn cmd_quote_neutralizes_output_redirection() {
        assert_eq!(cmd_quote("a > out.txt"), "\"a > out.txt\"");
    }

    #[test]
    fn cmd_quote_neutralizes_input_redirection() {
        assert_eq!(cmd_quote("a < in.txt"), "\"a < in.txt\"");
    }

    /// Doubling every embedded `"` is what stops a value from closing the
    /// quoted region early and resuming as bare (unneutralized) cmd.exe syntax
    /// — the classic quote-breakout attempt.
    #[test]
    fn cmd_quote_doubles_embedded_quotes_to_prevent_breakout() {
        assert_eq!(cmd_quote("a\"b"), "\"a\"\"b\"");
        let breakout_attempt = "foo\" & calc.exe & \"bar";
        assert_eq!(cmd_quote(breakout_attempt), "\"foo\"\" & calc.exe & \"\"bar\"");
    }

    /// cmd.exe still expands `%VAR%` *inside* a double-quoted region (unlike
    /// `&`/`|`/etc., which quoting alone neutralizes), so `cmd_quote` doubles
    /// every `%` to turn it into a literal percent rather than a variable
    /// reference.
    #[test]
    fn cmd_quote_escapes_percent_variable_expansion() {
        assert_eq!(cmd_quote("100%done"), "\"100%%done\"");
        assert_eq!(cmd_quote("%PATH%"), "\"%%PATH%%\"");
        assert_eq!(cmd_quote("%CD%"), "\"%%CD%%\"");
    }

    /// An already-doubled `%%` in the source value must still have *each* `%`
    /// doubled independently — the replacement is per-character, not
    /// pattern-aware, so two input percents become four output percents.
    #[test]
    fn cmd_quote_doubles_each_percent_in_already_doubled_input() {
        assert_eq!(cmd_quote("100%%done"), "\"100%%%%done\"");
    }

    #[test]
    fn cmd_quote_preserves_plain_filename_intact() {
        let quoted = cmd_quote("foo.rs");
        assert_eq!(quoted, "\"foo.rs\"");
        assert_eq!(quoted.trim_matches('"'), "foo.rs");
    }

    #[test]
    fn cmd_quote_preserves_windows_path_with_backslashes_intact() {
        let quoted = cmd_quote("src\\main.rs");
        assert_eq!(quoted, "\"src\\main.rs\"");
        assert_eq!(quoted.trim_matches('"'), "src\\main.rs");
    }

    /// A run of backslashes only acts as an escape when a `"` follows it, so the
    /// run that runs up against poly's own closing quote has to be doubled or it
    /// swallows that quote and the quoted region never terminates. Odd and even
    /// runs are both checked: doubling has to be unconditional, not a parity fix.
    #[test]
    fn cmd_quote_doubles_the_backslash_run_before_the_closing_quote() {
        let cases = [
            ("C:\\build\\", "\"C:\\build\\\\\""),
            ("C:\\build\\\\", "\"C:\\build\\\\\\\\\""),
            ("C:\\build\\\\\\", "\"C:\\build\\\\\\\\\\\\\""),
            ("\\", "\"\\\\\""),
        ];
        for (input, expected) in cases {
            assert_eq!(cmd_quote(input), expected, "input: {input:?}");
        }
    }

    /// The same rule applies to the run in front of an *embedded* quote: without
    /// doubling, `a\"` would reach the callee's argv parser as an escaped quote
    /// and merge the following text into the same argument.
    #[test]
    fn cmd_quote_doubles_the_backslash_run_before_an_embedded_quote() {
        assert_eq!(cmd_quote("a\\\"b"), "\"a\\\\\"\"b\"");
        assert_eq!(cmd_quote("a\\\\\"b"), "\"a\\\\\\\\\"\"b\"");
    }

    /// The doubling must be scoped to those two positions only — a blanket
    /// escape would rewrite every ordinary Windows path poly passes.
    #[test]
    fn cmd_quote_leaves_interior_backslashes_untouched() {
        assert_eq!(cmd_quote("C:\\src\\main.rs"), "\"C:\\src\\main.rs\"");
        assert_eq!(cmd_quote("C:\\\\server\\share\\file"), "\"C:\\\\server\\share\\file\"");
        // A `%` is not a quote, so the run in front of it is left alone while the
        // percent itself is still escaped.
        assert_eq!(cmd_quote("C:\\dir\\%PATH%"), "\"C:\\dir\\%%PATH%%\"");
    }

    /// The breakout this defect enables, in the shape it actually occurs: the
    /// Windows `shell_command` concatenates quoted tokens, so a token ending in
    /// a backslash would swallow its own closing quote and leave everything
    /// after it — here a `&` chain — outside any quoted region.
    #[test]
    fn a_trailing_backslash_token_does_not_unquote_the_token_after_it() {
        let joined = format!("{} {}", cmd_quote("C:\\build\\"), cmd_quote("x & calc.exe"));
        assert_eq!(joined, "\"C:\\build\\\\\" \"x & calc.exe\"");
    }

    #[test]
    fn cmd_quote_wraps_empty_string_in_a_pair_of_quotes() {
        assert_eq!(cmd_quote(""), "\"\"");
    }

    #[test]
    fn cmd_quote_wraps_whitespace_only_value_intact() {
        assert_eq!(cmd_quote("   "), "\"   \"");
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
