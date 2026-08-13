//! The [`Job`] and [`JobCache`] types — one runnable unit within a stage.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer};

use crate::HookCacheMode;
use crate::hooks::patterns::{Guard, Patterns};

/// One runnable unit within a stage (lefthook "command" or "script").
///
/// A job runs exactly one of `run` (a shell command) **xor** `script` (a script
/// file interpreted by `runner`); [`super::HooksConfig::validate`] enforces this.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Job {
    /// Display name; defaults to the map key when defined under
    /// `[hooks.<stage>.commands.<name>]` / `.scripts.<name>`.
    pub name: Option<String>,
    /// Shell command to run. Mutually exclusive with `script`.
    pub run: Option<String>,
    /// Script file to run. Mutually exclusive with `run`; requires `runner`.
    pub script: Option<String>,
    /// Interpreter used to execute `script` (e.g. `bash`, `python`).
    pub runner: Option<String>,
    /// Extra arguments appended to the invocation.
    pub args: Vec<String>,
    /// Glob(s) selecting which changed files this job receives.
    pub glob: Option<Patterns>,
    /// File include glob(s) (alias-style scoping alongside `glob`).
    pub files: Option<Patterns>,
    /// File exclude glob(s).
    pub exclude: Option<Patterns>,
    /// File-type filters (e.g. `text`, `executable`).
    pub file_types: Vec<String>,
    /// Run the job from this subdirectory.
    pub root: Option<String>,
    /// Skip guard — when active, the job does not run.
    pub skip: Option<Guard>,
    /// Only guard — the job runs *only* when active.
    pub only: Option<Guard>,
    /// Applicability probe for this job alone: exit 0 runs it, non-zero reports
    /// it **skipped** (visible, not a failure) while its siblings run normally.
    ///
    /// Prefer this to the stage-wide `[hooks.<stage>] precondition` whenever the
    /// prerequisite belongs to one tool — a stage-wide guard withholds every
    /// check in the stage. It is evaluated in the tree the job itself runs in,
    /// which under staged isolation is the staged snapshot, not the worktree.
    pub precondition: Option<String>,
    /// Setup command(s) for this job alone, run sequentially before it.
    ///
    /// A failure marks **this job** as having an unknown verdict and fails the
    /// stage, without preventing sibling jobs from reporting theirs. Same tree
    /// as `precondition`.
    pub before: Option<Patterns>,
    /// Tags for selective inclusion/exclusion.
    pub tags: Vec<String>,
    /// Per-job environment variables (merged over the global `[hooks].env`).
    pub env: BTreeMap<String, String>,
    /// Message printed when the job fails.
    pub fail_text: Option<String>,
    /// How long this job may run before poly kills it and reports it as timed
    /// out (rather than as having failed on its own merits).
    ///
    /// Accepts whole seconds (`timeout = 90`), a suffixed duration
    /// (`"500ms"`, `"30s"`, `"10m"`, `"1h"`), or `0` / `"off"` / `"none"` to run
    /// the job unbounded. Unset means the shape-derived default: a long budget
    /// for a per-file job, a far longer one for a `workspace` job.
    ///
    /// Held as raw text and resolved during lowering, where the job's name is
    /// known and a malformed value can name it. The environment overrides
    /// (`POLY_HOOK_TIMEOUT` / `POLY_HOOK_WORKSPACE_TIMEOUT`) outrank this key —
    /// see the `poly-hooks` `timeout` module for the full precedence chain.
    #[serde(default, deserialize_with = "deserialize_timeout")]
    pub timeout: Option<String>,
    /// Lower values run first within a stage (default `0`).
    pub priority: i64,
    /// Mutual-exclusion set this job belongs to.
    ///
    /// Hooks in a stage run concurrently; `serial` is the opt-out for a job that
    /// cannot tolerate a *peer* running at the same time — a tool holding a
    /// global lock, or two jobs writing the same output. It does **not** stop
    /// the run: a serial job still runs alongside every hook outside its set.
    ///
    /// - `serial = true` — join the shared set (never concurrent with another
    ///   `serial = true` job).
    /// - `serial = "cargo"` — join a **named** set; only members of that name
    ///   exclude each other. `"cargo"` is the set the built-in cargo group uses,
    ///   so a job invoking cargo should name it (see [`crate::CargoHooks`]).
    /// - `serial = false` — explicitly concurrent, overriding a stage-level
    ///   `parallel = false` and the automatic cargo grouping.
    ///
    /// Unset lets the stage decide: `piped`/`parallel = false` serialize a
    /// per-file job, a `run` line invoking cargo joins the `"cargo"` set, and a
    /// `workspace = true` job otherwise runs concurrently.
    #[serde(default, deserialize_with = "deserialize_serial")]
    pub serial: Serial,
    /// When the job modifies files and exits 0, the runner `git add`s the
    /// matched files and continues; only a non-zero exit fails the stage.
    pub stage_fixed: bool,
    /// Whole-workspace job: it compiles or analyses the entire project (e.g.
    /// `cargo clippy`, a type checker like `pyrefly`) rather than a per-file
    /// set. Default `false` (per-file).
    ///
    /// This decides whether the job receives filenames, **not** which tree it
    /// runs in: under staged isolation every job — per-file included — runs
    /// against the non-destructive snapshot of the staged index and so never
    /// sees unstaged worktree edits or untracked files (ADR 0019).
    pub workspace: bool,
    /// The job needs an interactive terminal.
    pub interactive: bool,
    /// Feed matched file contents to the job on stdin.
    pub use_stdin: bool,
    /// Per-job result-cache declaration.
    pub cache: Option<JobCache>,
}

/// Which mutual-exclusion set a [`Job`] belongs to (the `serial` key).
///
/// Four states rather than a bool, because "not configured" and "configured
/// concurrent" resolve differently: the first falls through to the stage
/// default (and to the automatic cargo grouping), the second is the escape
/// hatch and has to survive both.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Serial {
    /// No `serial` key; the stage default decides.
    #[default]
    Unset,
    /// `serial = false` — run concurrently with everything, whatever the stage
    /// or the automatic grouping would have said.
    Off,
    /// `serial = true` — join the shared exclusion set.
    Shared,
    /// `serial = "<name>"` — join the named exclusion set.
    Named(String),
}

/// Accept `serial` written as either a bool (`true` / `false`) or a set name
/// (`"cargo"`).
///
/// The bool is what most jobs want ("do not run me next to another serial
/// job"); the string is what a *shared external resource* wants, and the two
/// spellings say the same kind of thing, so rejecting either on a type
/// technicality would be a worse error than the one it prevents — the same
/// judgement as [`deserialize_timeout`].
fn deserialize_serial<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Serial, D::Error> {
    struct SerialVisitor;

    impl<'de> Visitor<'de> for SerialVisitor {
        type Value = Serial;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a bool (`true` / `false`) or an exclusion-set name (`\"cargo\"`)")
        }

        fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
            Ok(if value { Serial::Shared } else { Serial::Off })
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            let name = value.trim();
            if name.is_empty() {
                return Err(E::custom("`serial` set name must not be empty"));
            }
            Ok(Serial::Named(name.to_string()))
        }
    }

    deserializer.deserialize_any(SerialVisitor)
}

/// Accept a `timeout` written as either a TOML string (`"30s"`) or a bare
/// integer (`90`, whole seconds), normalising both to the raw text the lowering
/// step parses.
///
/// Both spellings are natural — `timeout = 60` reads fine, and `timeout = "10m"`
/// is what makes a long budget legible — and rejecting either on a type
/// technicality would be a worse error than the one it prevents.
fn deserialize_timeout<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    struct TimeoutVisitor;

    impl<'de> Visitor<'de> for TimeoutVisitor {
        type Value = Option<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a duration string (`30s`, `10m`) or whole seconds as an integer")
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(Some(value.to_string()))
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
            Ok(Some(value.to_string()))
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
            Ok(Some(value.to_string()))
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
            deserializer.deserialize_any(TimeoutVisitor)
        }
    }

    deserializer.deserialize_option(TimeoutVisitor)
}

/// Per-job result-cache declaration.
///
/// `mode` reuses the crate-wide [`HookCacheMode`]; `inputs` lists glob sets the
/// command depends on; `compiler` opts the job into tier-2 sccache env
/// injection.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct JobCache {
    /// Override the cache mode for this job; `None` inherits `[cache.results]`.
    pub mode: Option<HookCacheMode>,
    /// Glob sets the command's output depends on (e.g.
    /// `["**/*.rs", "Cargo.toml", "rust-toolchain.toml"]`).
    pub inputs: Vec<Patterns>,
    /// Opt into tier-2 sccache env injection (`RUSTC_WRAPPER`, …).
    pub compiler: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_run_job() {
        let job: Job = toml::from_str(
            r#"
run = "cargo fmt --check"
priority = -1
tags = ["rust"]
"#,
        )
        .unwrap();
        assert_eq!(job.run.as_deref(), Some("cargo fmt --check"));
        assert_eq!(job.priority, -1);
        assert_eq!(job.tags, vec!["rust".to_string()]);
        assert!(job.script.is_none());
    }

    #[test]
    fn parses_job_cache_with_string_or_array_inputs() {
        let job: Job = toml::from_str(
            r#"
run = "cargo clippy"
[cache]
mode = "aggressive"
compiler = true
inputs = ["**/*.rs", "Cargo.toml"]
"#,
        )
        .unwrap();
        let cache = job.cache.expect("cache present");
        assert_eq!(cache.mode, Some(HookCacheMode::Aggressive));
        assert!(cache.compiler);
        assert_eq!(cache.inputs.len(), 2);
        assert_eq!(cache.inputs[0].as_slice(), &["**/*.rs".to_string()]);
    }

    /// Both spellings of a budget are accepted and kept verbatim for lowering
    /// to resolve.
    #[test]
    fn parses_timeout_as_a_duration_string_or_whole_seconds() {
        for (source, expected) in [
            (r#"run = "x""#, None),
            ("run = \"x\"\ntimeout = \"10m\"", Some("10m")),
            ("run = \"x\"\ntimeout = \"off\"", Some("off")),
            ("run = \"x\"\ntimeout = 90", Some("90")),
            ("run = \"x\"\ntimeout = 0", Some("0")),
        ] {
            let job: Job = toml::from_str(source).expect("parse job");
            assert_eq!(job.timeout.as_deref(), expected, "source: {source}");
        }
    }

    /// The value is not validated here — the job's name lives one level up, and
    /// an error that cannot name the job is not worth much.
    #[test]
    fn a_timeout_this_crate_cannot_interpret_still_parses() {
        let job: Job = toml::from_str("run = \"x\"\ntimeout = \"soon\"").expect("parse job");
        assert_eq!(job.timeout.as_deref(), Some("soon"));
    }

    /// Both spellings of an exclusion set are accepted, and the absent key stays
    /// distinguishable from an explicit `false`.
    #[test]
    fn parses_serial_as_a_bool_or_a_set_name() {
        for (source, expected) in [
            (r#"run = "x""#, Serial::Unset),
            ("run = \"x\"\nserial = true", Serial::Shared),
            ("run = \"x\"\nserial = false", Serial::Off),
            ("run = \"x\"\nserial = \"cargo\"", Serial::Named("cargo".to_string())),
            ("run = \"x\"\nserial = \" cargo \"", Serial::Named("cargo".to_string())),
        ] {
            let job: Job = toml::from_str(source).expect("parse job");
            assert_eq!(job.serial, expected, "source: {source}");
        }
    }

    #[test]
    fn an_empty_serial_set_name_is_rejected() {
        let result: Result<Job, _> = toml::from_str("run = \"x\"\nserial = \"\"");
        assert!(result.is_err(), "an unnamed exclusion set is not a set");
    }

    #[test]
    fn unknown_job_field_is_rejected() {
        let result: Result<Job, _> = toml::from_str(
            r#"run = "x"
bogus = true"#,
        );
        assert!(result.is_err(), "deny_unknown_fields must reject `bogus`");
    }
}
