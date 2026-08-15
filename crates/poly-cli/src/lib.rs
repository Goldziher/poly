//! Shared implementation behind the `poly` CLI.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use poly_config::BaseConfigResolver;
use poly_core::{Config, LintResult, RunOptions, Severity, Verbosity, report};

use crate::config_sources::RemoteExtendsResolver;

pub mod cache_cmd;
pub mod config_cmd;
pub mod config_sources;
pub mod doctor;
pub mod hooks;
pub mod migrate;
pub mod remote;
pub mod rules_cmd;
pub mod workspace_coverage;

pub use cache_cmd::{CacheArgs, run_cache};
pub use config_cmd::{ConfigArgs, run_config};
pub use doctor::{DoctorArgs, run_doctor};
pub use hooks::{HooksArgs, run_hooks};
pub use migrate::{MigrateArgs, run_migrate};
pub use rules_cmd::{RulesArgs, run_rules};

/// Install the process-wide `tracing` subscriber for the CLI binaries at the
/// default verbosity (info-level poly notices). Equivalent to
/// [`init_logging_with(false)`](init_logging_with).
pub fn init_logging() {
    init_logging_with(false);
}

/// Install the process-wide `tracing` subscriber for the CLI binaries.
///
/// Idempotent (first call wins; safe to call from every entry point). Logs to
/// **stderr** so they never pollute `--format json` on stdout. The default
/// filter surfaces poly's own info-level notices — e.g. the "toolchain not
/// found; using the generic tier" fallback — while keeping dependencies quiet.
/// When `debug` is set, the poly crates are widened to `debug` level. `RUST_LOG`
/// always overrides either default.
///
/// Because the subscriber is first-call-wins, callers that honor `--debug` must
/// invoke this **after** argument parsing — see the binary entry points.
pub fn init_logging_with(debug: bool) {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        let directives = if debug {
            "warn,poly_core=debug,poly_hooks=debug,poly_cache=debug,poly_cli=debug"
        } else {
            "warn,poly_core=info,poly_hooks=info,poly_cache=info,poly_cli=info"
        };
        EnvFilter::new(directives)
    });
    let _ = fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Output rendering format.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Colored, human-oriented output.
    Pretty,
    /// JSON.
    Json,
    /// TOON (Token-Oriented Object Notation).
    Toon,
}

/// Flags shared by both subcommands.
#[derive(Args)]
pub struct CommonArgs {
    /// Files or directories to process (default: current directory).
    pub paths: Vec<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Pretty)]
    pub format: OutputFormat,

    /// Path to a config file (default: nearest poly.toml).
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Disable the result cache.
    #[arg(long)]
    pub no_cache: bool,

    /// Number of parallel jobs (default: all logical cores).
    #[arg(short = 'j', long)]
    pub jobs: Option<usize>,

    /// Gitignore-style glob to exclude from discovery (repeatable). Merged with
    /// the config's `[discovery] exclude`. Example: `--exclude 'test_apps/**'`.
    ///
    /// Matching is gitignore-style, which means a pattern that does not start
    /// with `/` matches a directory of that name **at any depth**, not only at
    /// the root: `--exclude 'e2e/**'` prunes the top-level `e2e/` and also
    /// `src/test/java/io/xberg/e2e/`, where `e2e` is just a package name. Anchor
    /// the pattern with a leading slash to mean only the top-level directory:
    /// `--exclude '/e2e/**'`. Run `poly doctor` to see which rules are matching
    /// at more than one depth.
    #[arg(long = "exclude", value_name = "GLOB")]
    pub exclude: Vec<String>,

    /// Disable colored output.
    #[arg(long)]
    pub no_color: bool,

    /// Apply fixes in place: autofixes for `lint`, formatting for `fmt`. The
    /// default is a dry run that reports what would change and writes nothing.
    #[arg(long)]
    pub fix: bool,

    /// Show extra per-finding detail in `pretty` output: description, rule URL,
    /// and metadata, and list every skipped file instead of the first few. For
    /// `--format json`/`toon` the findings are always fully structured, so this
    /// only lifts the cap on the skip note printed to stderr.
    #[arg(long)]
    pub verbose: bool,

    /// Apply `[discovery] exclude` to explicitly named files as well as to the
    /// directory walk. This is the default and remains as a compatibility flag.
    #[arg(long, conflicts_with = "include_excluded")]
    pub force_exclude: bool,

    /// Check explicitly named files or directory roots even when they match the
    /// exclude set.
    ///
    /// This is an explicit safety override for one-off inspection. Exclusions
    /// below an included directory remain active.
    #[arg(long, conflicts_with = "force_exclude")]
    pub include_excluded: bool,

    /// Apply `--fix` to machine-generated files too.
    ///
    /// By default `--fix` reports on a file marked `DO NOT EDIT` / `@generated`
    /// but does not rewrite it: the change is reverted by the next generation
    /// run, and it can silence the diagnostic that was the only evidence of a
    /// generator bug.
    #[arg(long)]
    pub fix_generated: bool,

    /// Emit debug data: per-engine cache hit/miss and timing (shown in `pretty`,
    /// attached to `json`/`toon`), and raise log verbosity to `debug` on stderr.
    #[arg(long)]
    pub debug: bool,

    /// Fail the run (exit 2) if any file was skipped. Equivalent to
    /// `--max-skips 0`.
    ///
    /// A skipped file is one nothing inspected: a path named on the command line
    /// that no engine covers, or a file every routed backend declined. They are
    /// always reported; this makes them fatal for a gate that needs coverage to
    /// be total. The failure names every file it fired on.
    #[arg(long, conflicts_with = "max_skips")]
    pub deny_skips: bool,

    /// Fail the run (exit 2) if more than N files were skipped.
    ///
    /// The budgeted form of `--deny-skips`, for a repo with a known, bounded set
    /// of files poly cannot check: the gate holds the number steady instead of
    /// letting it grow unnoticed.
    #[arg(long, value_name = "N")]
    pub max_skips: Option<usize>,
}

/// Exit code for a run that could not verify what it was asked to.
///
/// Shared with the missing-path and unparseable-file paths: 0 is clean, 1 is
/// findings (or drift), and 2 is "this run did not check what you asked" —
/// callers legitimately treat 1 as success for `--fix`, so a coverage failure
/// must never land there.
const EXIT_NOT_VERIFIED: u8 = 2;

/// Print a machine-readable document to stdout, or turn a render failure into
/// the not-verified exit code. Returns that code on failure, for the caller to
/// propagate.
///
/// Nothing is printed on failure, deliberately. The renderers used to fall back
/// to `[]`, which is byte-identical to a clean run over zero findings — and
/// since the exit code is derived from the run rather than from the render, a
/// warning-only run whose report failed to serialize exited `0` with an empty
/// document. A gate reading either signal would have called that success.
fn emit_structured(rendered: Result<String, report::RenderError>) -> Result<(), u8> {
    match rendered {
        Ok(document) => {
            println!("{document}");
            Ok(())
        }
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("error: no machine-readable report was written; this run verified nothing");
            Err(EXIT_NOT_VERIFIED)
        }
    }
}

/// `poly lint` arguments.
#[derive(Args)]
pub struct LintArgs {
    /// Skip the whole-project phase (`cargo clippy` and the other configured
    /// whole-workspace tools). By default a whole-repository `poly lint` also
    /// runs the same whole-project tools a `pre-commit` hook would; this checks
    /// only the per-file tier. Equivalent to `[lint] workspace = false`.
    ///
    /// This is also what makes a run read-only. Without `--fix` poly applies no
    /// fixes, but the whole-project phase still *executes* those tools against
    /// the live worktree, and their own side effects (a refreshed lock file, a
    /// populated build or type-checker cache) are not poly's to control.
    #[arg(long)]
    pub no_workspace: bool,

    /// Run the whole-project phase even though explicit paths were given.
    ///
    /// A path-scoped `poly lint <paths>` runs only the per-file tier, because
    /// naming a file should not silently escalate to an unbounded whole-workspace
    /// `cargo` build. Pass this to opt back in — a commit gate that lints staged
    /// paths and wants clippy should set it explicitly.
    ///
    /// The phase itself is never path-scoped: the tools cover the whole
    /// repository regardless of the named paths and of `[discovery] exclude`. The
    /// run says so on stderr.
    #[arg(long, conflicts_with = "no_workspace")]
    pub workspace: bool,

    /// Flags shared with `poly fmt`.
    #[command(flatten)]
    pub common: CommonArgs,
}

/// `poly fmt` arguments.
#[derive(Args)]
pub struct FmtArgs {
    /// Explicit dry run (the default): report what would change, write nothing,
    /// exit non-zero if any file would change. Conflicts with `--fix`.
    #[arg(long, conflicts_with = "fix")]
    pub check: bool,

    /// Flags shared with `poly lint`.
    #[command(flatten)]
    pub common: CommonArgs,
}

/// Run the lint pipeline and map the outcome to a process exit code.
pub fn run_lint(args: LintArgs) -> ExitCode {
    let no_workspace = args.no_workspace;
    let force_workspace = args.workspace;
    let common = args.common;
    init_logging_with(common.debug);
    apply_color(&common);
    let verbosity = Verbosity::new(common.verbose, common.debug);
    let (paths, config, mut opts) = match prepare(&common) {
        Ok(triple) => triple,
        Err(code) => return code,
    };

    // Explicit paths scope the run. Without this, `poly lint some/file.py` looks
    // like a sub-second operation but escalates to an unbounded whole-workspace
    // cargo build — which, when another process holds the cargo package lock,
    // blocks indefinitely with nothing in the argument list to suggest why.
    //
    // But `poly lint .` is how people say "lint everything", so naming the root
    // is a request for the whole project, not a narrowing of it. Treating it as
    // path-scoped meant a repo whose CI ran `poly lint .` quietly got the weaker
    // check for several sessions and reported itself clean on that basis —
    // silent under-checking, the expensive failure direction.
    let narrowed_to_paths = !common.paths.is_empty() && !any_path_is_workspace_root(&common.paths);
    let path_scoped = narrowed_to_paths && !force_workspace;
    // Loaded *before* the per-file run, not after it: the per-file tier decides
    // there and then whether a file's language went unlinted, and a run that is
    // about to lint every `.rs` file with clippy must not first record them as
    // `no lint rules for Rust`. The same value drives the phase itself below, so
    // the coverage predicted here and the tools that run are read from one
    // config. `None` means the phase does not run at all; a load failure is
    // carried rather than raised, so the findings the per-file tier did produce
    // are still reported before the run fails on it.
    let workspace_config =
        (!no_workspace && !path_scoped).then(|| hooks::commands::load_config(common.config.as_deref()));
    opts.externally_linted_languages = match &workspace_config {
        Some(Ok(config)) => workspace_coverage::workspace_lint_languages(config),
        _ => Vec::new(),
    };

    let run = match poly_core::lint_run(&paths, &config, &opts, common.fix, common.debug) {
        Ok(run) => run,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::from(2);
        }
    };
    let results = &run.results;

    let pretty = matches!(common.format, OutputFormat::Pretty);
    let _ = match common.format {
        OutputFormat::Pretty => report::report_lint_pretty_run(&run, verbosity),
        OutputFormat::Json => {
            if let Err(code) = emit_structured(report::report_lint_json_run(&run)) {
                return ExitCode::from(code);
            }
            report::eprint_discovery_note(&run.discovery);
            report::eprint_skip_note(&run.skipped, common.verbose);
            report::eprint_lint_errors(&run.errors);
            results.iter().map(|r| r.diagnostics.len()).sum()
        }
        OutputFormat::Toon => {
            if let Err(code) = emit_structured(report::report_lint_toon_run(&run)) {
                return ExitCode::from(code);
            }
            report::eprint_discovery_note(&run.discovery);
            report::eprint_skip_note(&run.skipped, common.verbose);
            report::eprint_lint_errors(&run.errors);
            results.iter().map(|r| r.diagnostics.len()).sum()
        }
    };

    // Reported here, folded into the exit code at the end: a strictness flag
    // must change the verdict, not what else the run does.
    let skips_over_budget = report_skip_budget(&common, &run.skipped);

    let workspace_ok = match &workspace_config {
        // Only explain the skip when path scoping caused it. `--no-workspace` is
        // an explicit opt-out and needs no narration.
        None => {
            if path_scoped && !no_workspace && pretty {
                eprintln!("note: whole-project phase skipped for path-scoped run (pass --workspace to include it)");
            }
            true
        }
        Some(Ok(config)) => {
            // The mirror of the skip note above: `--workspace` reverses the
            // scoping the paths asked for, and the phase then covers the whole
            // repository — a whole-project tool has no file list to narrow (the
            // phase passes none) and `[discovery] exclude` filters poly's own
            // discovery, not what such a tool reads. Silence about that read as a
            // dropped filter to a consumer who saw findings from outside the paths
            // they named. Printed to stderr in every format, like the other notes,
            // so a `--format json` consumer sees it too.
            if narrowed_to_paths {
                eprintln!(
                    "note: whole-project phase covers the entire repository, not just the named paths \
                     (and not filtered by [discovery] exclude)"
                );
            }
            match run_workspace_phase(config, &common, pretty) {
                Ok(ok) => ok,
                Err(e) => {
                    eprintln!("error: whole-project lint phase failed: {e:#}");
                    return ExitCode::from(2);
                }
            }
        }
        Some(Err(e)) => {
            eprintln!("error: whole-project lint phase failed: {e:#}");
            return ExitCode::from(2);
        }
    };

    // A file whose engine failed was not checked, so it belongs with the other
    // coverage failures in 2 — never in 1, which callers treat as "findings to
    // fix" and, under `--fix`, as success. The findings that *were* produced are
    // still reported above; only the verdict changes.
    if skips_over_budget || !run.errors.is_empty() {
        ExitCode::from(EXIT_NOT_VERIFIED)
    } else if lint_has_errors(results) || !workspace_ok {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Apply `--deny-skips` / `--max-skips`, naming every file that put the run over
/// budget. Returns whether the caller must fail the run.
///
/// The names are the point. A gate that fails with "3 files were skipped" and no
/// paths reproduces the defect the flag exists to fix, one level up: the
/// consumer is again left reconstructing which files poly meant. Printed to
/// stderr so it survives `--format json` piping.
fn report_skip_budget(common: &CommonArgs, skipped: &[poly_core::SkippedFile]) -> bool {
    let Some(budget) = (if common.deny_skips { Some(0) } else { common.max_skips }) else {
        return false;
    };
    if skipped.len() <= budget {
        return false;
    }
    for entry in skipped {
        eprintln!("error: skipped {}: {}", entry.path.display(), entry.reason);
    }
    eprintln!(
        "error: refusing to report success for {} skipped file(s) (limit {budget})",
        skipped.len()
    );
    true
}

/// Run `poly lint`'s whole-project phase and render its report.
///
/// `config` is the poly config loaded exactly as `poly hooks` loads it —
/// honouring git-remote `extends` bases via the CLI's resolver — and is the same
/// value [`workspace_coverage::workspace_lint_languages`] was asked about before
/// the per-file run, so the tool set predicted there is the tool set run here.
/// Delegates the orchestration to the shared [`poly_workspace`] crate and prints
/// the outcome. Returns whether the phase passed (the caller folds a `false`
/// into a non-zero exit code). The `--no-workspace` short-circuit is handled by
/// the caller, before this runs.
fn run_workspace_phase(config: &poly_config::PolyConfig, common: &CommonArgs, pretty: bool) -> anyhow::Result<bool> {
    let outcome = poly_workspace::run_workspace_lint(
        config,
        &poly_workspace::WorkspaceLintOptions {
            fix: common.fix,
            jobs: common.jobs,
            no_cache: common.no_cache,
            report_to_stdout: pretty,
        },
    )?;
    poly_workspace::render_workspace_outcome(&outcome, pretty);
    Ok(outcome.passed)
}

/// Whether any diagnostic across all results is error-severity.
fn lint_has_errors(results: &[LintResult]) -> bool {
    results
        .iter()
        .any(|r| r.diagnostics.iter().any(|d| d.severity == Severity::Error))
}

/// Run the format pipeline and map the outcome to a process exit code.
pub fn run_fmt(args: FmtArgs) -> ExitCode {
    let common = &args.common;
    init_logging_with(common.debug);
    apply_color(common);
    let verbosity = Verbosity::new(common.verbose, common.debug);
    let (paths, config, opts) = match prepare(common) {
        Ok(triple) => triple,
        Err(code) => return code,
    };

    let write = common.fix;
    let (changed, format_errors, skips_over_budget) =
        match poly_core::format_run(&paths, &config, &opts, write, common.debug) {
            Ok(run) => {
                let errors = run.errors.len();
                let changed = match common.format {
                    OutputFormat::Pretty => report::report_format_pretty_run(&run, !write, verbosity),
                    OutputFormat::Json => {
                        if let Err(code) = emit_structured(report::report_format_json_run(&run)) {
                            return ExitCode::from(code);
                        }
                        report::eprint_discovery_note(&run.discovery);
                        report::eprint_skip_note(&run.skipped, common.verbose);
                        report::eprint_format_errors(&run.errors);
                        run.results.iter().filter(|r| r.changed).count()
                    }
                    OutputFormat::Toon => {
                        if let Err(code) = emit_structured(report::report_format_toon_run(&run)) {
                            return ExitCode::from(code);
                        }
                        report::eprint_discovery_note(&run.discovery);
                        report::eprint_skip_note(&run.skipped, common.verbose);
                        report::eprint_format_errors(&run.errors);
                        run.results.iter().filter(|r| r.changed).count()
                    }
                };
                (changed, errors, report_skip_budget(common, &run.skipped))
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                return ExitCode::from(2);
            }
        };

    // `poly fmt` is a pure formatter: it runs only the per-file formatting tier
    // above and never the whole-project lint phase (`cargo clippy`/`-sort`/
    // `-machete`/`-deny`). Those are linting, not formatting — they belong to
    // `poly lint` (and the commit gate), never to `fmt`.
    //
    // Exit codes are a contract consumers automate against: 0 = clean, 1 =
    // files changed (or would change), 2 = the run failed. A file the engine
    // could not parse belongs in 2, never in 0 — it was not verified — and
    // never in 1, which callers legitimately treat as success for `--fix`.
    // A skip budget breach lands in 2 for the same reason a parse failure does:
    // those files were not verified, and 1 is a code callers treat as success.
    if format_errors > 0 || skips_over_budget {
        ExitCode::from(EXIT_NOT_VERIFIED)
    } else if changed > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn apply_color(common: &CommonArgs) {
    if common.no_color {
        owo_colors::set_override(false);
    }
}

/// Resolve paths, load config, and build run options; on config failure return
/// the exit code to propagate.
fn prepare(common: &CommonArgs) -> Result<(Vec<PathBuf>, Config, RunOptions), ExitCode> {
    let paths = if common.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        common.paths.clone()
    };
    reject_missing_paths(&paths)?;
    // Build the remote `extends` resolver rooted at the repository so both the
    // top-level load and the runner's nested-config resolution honor shared bases.
    let resolver = match config_sources::repo_root().and_then(|root| RemoteExtendsResolver::new(&root)) {
        Ok(resolver) => std::sync::Arc::new(resolver),
        Err(e) => {
            eprintln!("error: failed to prepare config resolver: {e:#}");
            return Err(ExitCode::from(2));
        }
    };
    let config = match load_config(common.config.as_deref(), resolver.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to load config: {e:#}");
            return Err(ExitCode::from(2));
        }
    };
    let opts = RunOptions {
        no_cache: common.no_cache,
        jobs: common.jobs,
        exclude: common.exclude.clone(),
        force_exclude: !common.include_excluded,
        fix_generated: common.fix_generated,
        explicit_config: common.config.is_some(),
        config_resolver: Some(resolver),
        // Filled in by `run_lint` once it knows whether the whole-project phase
        // runs; `poly fmt` never runs that phase, so it keeps the empty default.
        externally_linted_languages: Vec::new(),
    };
    Ok((paths, config, opts))
}

/// Whether any path argument names the directory poly is running in — i.e. the
/// caller asked for the whole project rather than a subset of it.
///
/// `poly lint .` is the ordinary way to say "lint everything", so it must behave
/// like `poly lint` with no arguments. Compared canonically so `.`, `./`, an
/// absolute path to the same directory, and `$PWD` all agree; a path that cannot
/// be canonicalized is simply not the root, which errs toward the narrower,
/// cheaper run rather than silently escalating.
fn any_path_is_workspace_root(paths: &[PathBuf]) -> bool {
    let Ok(cwd) = std::env::current_dir().and_then(|p| p.canonicalize()) else {
        return false;
    };
    paths
        .iter()
        .filter_map(|path| path.canonicalize().ok())
        .any(|path| path == cwd)
}

/// Fail the run when a path argument does not exist.
///
/// A missing path used to be discarded silently by the walker, so
/// `poly fmt --check typo.py` reported `All formatted. (0 file(s) scanned)` and
/// exited 0 — a green result that verified nothing. Worse, a mix of real and
/// missing paths checked only the real ones and still exited 0, so the file
/// count looked plausible. A hook or CI step feeding poly a stale path list was
/// indistinguishable from a passing gate.
fn reject_missing_paths(paths: &[PathBuf]) -> Result<(), ExitCode> {
    let missing: Vec<&PathBuf> = paths.iter().filter(|p| !p.exists()).collect();
    if missing.is_empty() {
        return Ok(());
    }
    for path in &missing {
        eprintln!("error: path does not exist: {}", path.display());
    }
    eprintln!(
        "error: refusing to report success for {} unreadable path argument(s)",
        missing.len()
    );
    Err(ExitCode::from(2))
}

fn load_config(explicit: Option<&Path>, resolver: &dyn BaseConfigResolver) -> anyhow::Result<Config> {
    match explicit {
        Some(p) => Config::load_file_with(p, resolver),
        None => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            Config::load_with(&cwd, resolver)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use poly_core::Diagnostic;

    fn diag(severity: Severity) -> Diagnostic {
        Diagnostic {
            engine: "test".to_string(),
            code: None,
            severity,
            title: "test finding".to_string(),
            description: None,
            span: None,
            url: None,
            fix: Vec::new(),
            metadata: std::collections::BTreeMap::new(),
        }
    }

    fn result(diagnostics: Vec<Diagnostic>) -> LintResult {
        LintResult {
            path: PathBuf::from("test.rs"),
            diagnostics,
            fix_withheld_generated: false,
            fixed: 0,
            skipped: None,
            error: None,
            debug: None,
        }
    }

    #[test]
    fn no_diagnostics_yields_success() {
        assert!(!lint_has_errors(&[result(vec![])]));
    }

    #[test]
    fn warning_only_diagnostics_yield_success() {
        let results = vec![result(vec![
            diag(Severity::Warning),
            diag(Severity::Info),
            diag(Severity::Hint),
        ])];
        assert!(
            !lint_has_errors(&results),
            "warning/info/hint findings must not fail the run"
        );
    }

    #[test]
    fn error_diagnostic_yields_failure() {
        let results = vec![result(vec![diag(Severity::Warning), diag(Severity::Error)])];
        assert!(lint_has_errors(&results), "an error-severity finding must fail the run");
    }

    #[test]
    fn error_in_any_result_yields_failure() {
        let results = vec![
            result(vec![diag(Severity::Warning)]),
            result(vec![diag(Severity::Error)]),
        ];
        assert!(lint_has_errors(&results));
    }

    /// A `LintResult` whose path is valid to the OS but cannot be encoded as
    /// JSON — the real trigger for a render failure, since a Linux checkout may
    /// legally hold a non-UTF-8 filename and poly's discovery walks it.
    #[cfg(unix)]
    fn unrenderable_result() -> LintResult {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        LintResult {
            path: PathBuf::from(OsString::from_vec(vec![b'b', b'a', b'd', 0xff, b'.', b'p', b'y'])),
            diagnostics: vec![diag(Severity::Warning)],
            fix_withheld_generated: false,
            fixed: 0,
            skipped: None,
            error: None,
            debug: None,
        }
    }

    /// The false-pass this guards: the renderer used to swallow the failure and
    /// return `[]`, and the exit code is computed from the run rather than the
    /// render — so a warning-only run exited `0` with an empty document, which is
    /// byte-identical to a clean run. The exit code must now say the run failed.
    #[cfg(unix)]
    #[test]
    fn structured_render_failure_yields_the_not_verified_exit_code() {
        let results = vec![unrenderable_result()];
        assert!(
            !lint_has_errors(&results),
            "the run itself must be clean, so only the render can fail the process"
        );

        let rendered = report::report_lint_json(&results);
        assert!(rendered.is_err(), "a non-UTF-8 path must fail to render as JSON");
        assert_eq!(
            emit_structured(rendered),
            Err(EXIT_NOT_VERIFIED),
            "a failed render must not report success"
        );
    }

    #[cfg(unix)]
    #[test]
    fn structured_toon_render_failure_yields_the_not_verified_exit_code() {
        let rendered = report::report_lint_toon(&[unrenderable_result()]);
        assert!(rendered.is_err(), "a non-UTF-8 path must fail to render as TOON");
        assert_eq!(emit_structured(rendered), Err(EXIT_NOT_VERIFIED));
    }

    #[test]
    fn a_rendered_document_is_emitted_and_reports_success() {
        let rendered = report::report_lint_json(&[result(vec![diag(Severity::Warning)])]);
        assert_eq!(emit_structured(rendered), Ok(()));
    }
}
