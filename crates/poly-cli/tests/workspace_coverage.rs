//! `poly lint` must not contradict itself about what it linted.
//!
//! The per-file tier held no Rust rules, so a `.rs` file left it uncovered.
//! But `poly lint` also runs a whole-project phase, and that phase runs
//! `cargo clippy`. The first release of the coverage accounting reported both:
//! 229 lines of `skipped …: no lint rules for Rust`, and then, in the same
//! output, `✓ cargo-clippy`. Rust *was* linted. "No lint rules for Rust" is a
//! true statement about the per-file tier and a false one about the run — which
//! is this project's defining defect inverted, a claim that something was not
//! checked when it was.
//!
//! The other half matters just as much: with the whole-project phase off
//! (`--no-workspace`, or a repo that configures no whole-project tools) nothing
//! lints Rust, so the skip is accurate and has to survive.
//!
//! # Why these fixtures switch the per-file Rust tier off
//!
//! They no longer *can* reach the contradicting state on poly's shipped
//! defaults, and that is a coverage win rather than a problem with the
//! mechanism. Rust now has two per-file lint sources — the `quality` tier's
//! structural model (ADR 0027) and the built-in ast-grep pack's `unwrap-used`
//! and friends — so a `.rs` file is covered before the whole-project phase is
//! consulted at all, and there is no skip left for that phase to retract.
//! `externally_linted_languages` is therefore inert under the defaults; the
//! two languages it maps (Rust via clippy, Go via golangci-lint) are both
//! covered per-file today.
//!
//! It is still live for a repo that turns those off, which is exactly what
//! `[lint.quality] enabled = false` + `[rules] builtin = false` reconstructs
//! below. The alternative — picking some other language for the fixtures — is
//! not available: no language poly maps to a whole-project tool lacks per-file
//! coverage any more.
//!
//! These shell out to the built binary because the contradiction only exists in
//! the assembled output — both phases, one stream.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// The claim under test, verbatim.
const NO_RUST_RULES: &str = "no lint rules for Rust";

/// A whole-project phase that runs one tool, under the id poly's own cargo
/// builtin uses.
///
/// The command is `true` rather than a real `cargo clippy` invocation on
/// purpose: what is under test is whether the two phases agree about coverage,
/// not whether clippy works, and compiling a crate per case would cost seconds
/// to prove nothing extra. `cargo = false` keeps the real cargo builtin group
/// out, so the tool set is exactly the one written here.
const WORKSPACE_HOOKS: &str = r#"
[lint.quality]
enabled = false

[rules]
builtin = false

[hooks]
stages = ["pre-commit"]

[hooks.builtin]
cargo = false

[hooks.pre-commit.commands.cargo-clippy]
run = "true"
workspace = true
"#;

/// The two per-file sources of Rust lint coverage, switched off so a `.rs`
/// file is genuinely uncovered by the per-file tier and the whole-project
/// phase is the only thing that can lint it — the state these fixtures exist
/// to test the reporting of (see the module docs). `quality` (ADR 0027)
/// structurally models Rust, and the built-in ast-grep pack ships five
/// default-on Rust rules; either alone would make the file covered.
///
/// Written to an out-of-tree `--config` file rather than a `poly.toml` inside
/// the walked directory so it cannot itself become a counted or
/// excluded-and-noted file and shift the exact strings asserted below.
fn disable_per_file_rust_config() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("poly.toml"),
        "[lint.quality]\nenabled = false\n\n[rules]\nbuiltin = false\n",
    )
    .expect("write config");
    dir
}

fn write(dir: &TempDir, name: &str, body: &str) {
    std::fs::write(dir.path().join(name), body).expect("write fixture");
}

/// A repo with no `[hooks]` section at all, so the whole-project phase has
/// nothing to run and `lib.rs` is the only file in the walk.
fn repo_without_workspace_phase() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    write(&dir, "lib.rs", "pub fn main() {}\n");
    dir
}

/// The same `lib.rs`, plus the config that gives the run a whole-project phase.
/// The `poly.toml` is itself a file the per-file tier lints, which is why the
/// counts below are two rather than one.
fn repo_with_clippy() -> TempDir {
    let dir = repo_without_workspace_phase();
    write(&dir, "poly.toml", WORKSPACE_HOOKS);
    dir
}

/// Run `poly` with the per-file Rust tier disabled via an out-of-tree
/// `--config` file (see [`disable_per_file_rust_config`]). `--config`
/// *replaces* the normal config lookup rather than layering underneath it, so
/// this is only for `repo_without_workspace_phase`, which has no `poly.toml`
/// of its own — `repo_with_clippy` disables the same two inline in its own
/// `WORKSPACE_HOOKS`, since it already needs its `poly.toml` read for the
/// whole-project `[hooks]`.
fn poly_per_file_rust_disabled(root: &Path, args: &[&str]) -> Output {
    let config_dir = disable_per_file_rust_config();
    let config_path = config_dir.path().join("poly.toml");
    let config_path = config_path.to_str().expect("utf8 path").to_owned();
    // `--config` is a per-subcommand flag (`poly lint --config <path>`, not
    // `poly --config <path> lint`), so it must be inserted after `args[0]`
    // (the subcommand) rather than prepended.
    let (&subcommand, rest) = args.split_first().expect("at least a subcommand");
    let mut full_args: Vec<&str> = vec![subcommand, "--config", &config_path];
    full_args.extend_from_slice(rest);
    poly(root, &full_args)
}

fn poly(root: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .args(args)
        .current_dir(root)
        .output()
        .expect("run poly")
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The defect: a run whose whole-project phase lints Rust must not report Rust
/// as unlinted. Both halves are asserted together — the absence of the claim
/// only means something beside the evidence that the phase did run.
#[test]
fn a_run_that_lints_rust_in_the_whole_project_phase_does_not_call_rust_unlinted() {
    let dir = repo_with_clippy();
    let output = poly(dir.path(), &["lint", "--no-cache", "--no-color", "."]);
    let text = combined(&output);

    assert!(
        text.contains("✓ cargo-clippy"),
        "the whole-project phase must have run for this test to mean anything, got:\n{text}"
    );
    assert!(
        !text.contains(NO_RUST_RULES),
        "the run linted Rust and must not say otherwise, got:\n{text}"
    );
}

/// Not merely quieter: the file is genuinely counted, so the note, the count,
/// the JSON payload and `--deny-skips` all describe the same run. Suppressing
/// the line while leaving the count at one would be the display-only fix this
/// release exists to rule out.
#[test]
fn a_rust_file_covered_by_the_whole_project_phase_is_counted_as_linted() {
    let dir = repo_with_clippy();
    let output = poly(dir.path(), &["lint", "--no-cache", "--no-color", "."]);
    let text = combined(&output);

    assert!(
        text.starts_with("No issues found.\n  2 files linted\n"),
        "lib.rs and poly.toml, nothing skipped, got:\n{text}"
    );
}

/// The same claim, machine-readable: the JSON document must not carry a skip
/// entry the human output has stopped printing.
#[test]
fn json_carries_no_rust_skip_when_the_whole_project_phase_covers_it() {
    let dir = repo_with_clippy();
    let output = poly(
        dir.path(),
        &["lint", "--no-cache", "--no-color", "--format", "json", "."],
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout stays valid JSON");

    assert_eq!(
        value.as_array().expect("top level stays an array").len(),
        0,
        "a covered file with no findings is not an entry: {stdout}"
    );
    assert!(
        !combined(&output).contains(NO_RUST_RULES),
        "got:\n{}",
        combined(&output)
    );
}

/// `--deny-skips` must agree with the note. A skip suppressed for display only
/// would still fail this gate, which is how a display-only fix gets caught.
#[test]
fn deny_skips_passes_when_the_whole_project_phase_covers_the_language() {
    let dir = repo_with_clippy();
    let output = poly(dir.path(), &["lint", "--no-cache", "--no-color", "--deny-skips", "."]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "the language was linted, so there is no skip to deny: {}",
        combined(&output)
    );
}

/// The other half. With the phase off nothing in the run lints Rust, so the skip
/// is accurate and must still appear, name the file, and stay out of the count.
#[test]
fn no_workspace_keeps_the_rust_skip_because_nothing_lints_rust_then() {
    let dir = repo_with_clippy();
    let output = poly(dir.path(), &["lint", "--no-workspace", "--no-cache", "--no-color", "."]);
    let text = combined(&output);

    assert_eq!(
        text,
        concat!(
            "  skipped ./lib.rs: no lint rules for Rust\n",
            "\n",
            "No issues found.\n",
            "  1 file linted\n",
            "  1 file skipped: no lint rules for Rust\n",
        )
    );
}

/// A repo that configures no whole-project tools at all reaches the same state
/// without the flag: the phase does not run, so nothing lints Rust — and with
/// nothing left to count, the headline says so rather than reading as clean.
#[test]
fn a_repo_with_no_hooks_config_keeps_the_rust_skip() {
    let dir = repo_without_workspace_phase();
    let output = poly_per_file_rust_disabled(dir.path(), &["lint", "--no-cache", "--no-color", "."]);
    let text = combined(&output);

    assert_eq!(
        text,
        concat!(
            "  skipped ./lib.rs: no lint rules for Rust\n",
            "\n",
            "Nothing was linted.\n",
            "  0 files linted\n",
            "  1 file skipped: no lint rules for Rust\n",
        )
    );
}

/// `--deny-skips` fires on the accurate skip, so the strict gate keeps working
/// for the case it was built for.
#[test]
fn deny_skips_still_fires_on_rust_without_the_whole_project_phase() {
    let dir = repo_with_clippy();
    let output = poly(
        dir.path(),
        &[
            "lint",
            "--no-workspace",
            "--no-cache",
            "--no-color",
            "--deny-skips",
            ".",
        ],
    );
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(2), "got:\n{text}");
    assert!(
        text.contains("error: skipped ./lib.rs: no lint rules for Rust"),
        "got:\n{text}"
    );
}

/// A language *nothing* in the run lints keeps its skip even while the
/// whole-project phase covers another language. Crediting the phase must not
/// become a blanket amnesty.
#[test]
fn a_language_nothing_lints_keeps_its_skip_beside_a_covered_one() {
    let dir = repo_with_clippy();
    write(&dir, "a.zig", "pub fn main() void {}\n");

    let output = poly(dir.path(), &["lint", "--no-cache", "--no-color", "."]);
    let text = combined(&output);

    assert!(
        text.contains("No issues found.\n  2 files linted\n  1 file skipped: no lint rules for Zig"),
        "Rust is covered by the phase, Zig is covered by nothing, got:\n{text}"
    );
    assert!(
        text.contains("  skipped ./a.zig: no lint rules for Zig"),
        "got:\n{text}"
    );
    assert!(!text.contains(NO_RUST_RULES), "got:\n{text}");
}

/// …and `--deny-skips` still sees it, which is the whole reason an uncovered
/// language has to stay in the skipped set rather than merely be mentioned.
#[test]
fn deny_skips_fires_on_zig_while_the_whole_project_phase_covers_rust() {
    let dir = repo_with_clippy();
    write(&dir, "a.zig", "pub fn main() void {}\n");

    let output = poly(dir.path(), &["lint", "--no-cache", "--no-color", "--deny-skips", "."]);
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(2), "got:\n{text}");
    assert!(
        text.contains("error: skipped ./a.zig: no lint rules for Zig"),
        "got:\n{text}"
    );
    assert!(
        text.contains("refusing to report success for 1 file skipped"),
        "the covered Rust file must not be in the failing set, got:\n{text}"
    );
}

/// Nine Zig files and nothing else, so the note has exactly one reason to
/// report. Zig needs no config to stay uncovered — it has no tier-1 backend,
/// no construct table in `quality`, and no built-in ast-grep rule — so these
/// two run against poly's shipped defaults.
fn uncovered_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..9 {
        write(&dir, &format!("a{i}.zig"), "pub fn main() void {}\n");
    }
    dir
}

/// The note groups by reason: many files sharing one reason collapse to a count
/// and a sample instead of one line each. 229 identical lines is not a report.
#[test]
fn a_bulk_reason_is_aggregated_in_the_end_to_end_note() {
    let dir = uncovered_repo();
    let output = poly(dir.path(), &["lint", "--no-workspace", "--no-cache", "--no-color", "."]);
    let text = combined(&output);

    assert_eq!(
        text,
        concat!(
            "  skipped 9 files: no lint rules for Zig\n",
            "    e.g. ./a0.zig, ./a1.zig, ./a2.zig\n",
            "  pass --verbose to list every skipped file, or --format json for the full set\n",
            "\n",
            "Nothing was linted.\n",
            "  0 files linted\n",
            "  9 files skipped: no lint rules for Zig\n",
        )
    );
}

/// `--verbose` opts out of the grouping and names every file, so the aggregated
/// view never becomes the only view.
#[test]
fn verbose_expands_the_aggregated_note() {
    let dir = uncovered_repo();
    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--no-color", "--verbose", "."],
    );
    let text = combined(&output);

    assert_eq!(
        text.matches("  skipped ").count(),
        9,
        "one line per file under --verbose, got:\n{text}"
    );
    assert!(
        text.contains("  skipped ./a8.zig: no lint rules for Zig"),
        "got:\n{text}"
    );
}
