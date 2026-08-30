//! A binary file is not undecodable text — it is a file poly should never have
//! opened as text, and the two must not report the same way.
//!
//! `poly fmt --check` over Django exited 2 because 1,263 compiled `.mo`
//! catalogs each produced a "not valid UTF-8" per-file **error**, and `poly
//! lint` exited 1 on the same files because the decode failure is raised as an
//! error-severity `invalid-utf8` diagnostic. Neither is a defect in the
//! repository: a `.mo` is *supposed* to be bytes. Compiled artifacts sitting in
//! a tree therefore made a clean repository un-gateable.
//!
//! The distinction has to stay sharp in the other direction too, which is why
//! the second half of this file asserts the old behaviour is intact: a *text*
//! file that is genuinely malformed UTF-8 is a real finding, it is what
//! `tests/invalid_utf8.rs` covers, and widening the binary skip to swallow it
//! would hide exactly the corruption poly exists to report.

use poly_core::{Config, RunOptions};

/// A GNU gettext `.mo` catalog: the real magic number, then a NUL-bearing body.
/// Binary by the only property that matters here — it carries NUL bytes — while
/// still being plausible as the artifact the defect was found on.
const COMPILED_CATALOG: &[u8] = b"\xde\x12\x04\x95\x00\x00\x00\x00\x01\x00\x00\x00\x1c\x00\x00\x00msgid\x00msgstr\x00";

/// Text that is not valid UTF-8 and carries **no** NUL byte — the case that
/// must keep erroring.
const MALFORMED_TEXT: &[u8] = b"x = 1\n\xff\xfe not utf-8\n";

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        generated: None,
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
    }
}

#[test]
fn a_binary_file_is_skipped_by_lint_rather_than_reported_as_an_error() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let path = directory.path().join("django.mo");
    std::fs::write(&path, COMPILED_CATALOG).expect("write compiled catalog");

    let run = poly_core::lint_run(
        &[directory.path().to_path_buf()],
        &Config::default(),
        &options(),
        false,
        false,
    )
    .expect("lint run completes");

    assert!(run.errors.is_empty(), "a binary file is not a failure to lint");
    assert_eq!(run.checked, 0, "nothing was linted");
    assert!(
        run.results.is_empty(),
        "a compiled artifact must not produce an error-severity diagnostic that fails the run"
    );

    // A skipped file leaves the run through `skipped`, not `results` — the same
    // channel the generated-file opt-out uses, so `--deny-skips` sees it.
    assert_eq!(run.skipped.len(), 1, "the skip is accounted for, not silently dropped");
    assert_eq!(run.skipped[0].path, path);
    assert_eq!(
        run.skipped[0].reason, "binary file",
        "the skip names what poly saw, so a reader can tell it from malformed text"
    );
}

#[test]
fn a_binary_file_is_skipped_by_format_rather_than_failing_the_check() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let path = directory.path().join("django.mo");
    std::fs::write(&path, COMPILED_CATALOG).expect("write compiled catalog");

    let run = poly_core::format_run(
        &[directory.path().to_path_buf()],
        &Config::default(),
        &options(),
        true,
        false,
    )
    .expect("format run completes");

    assert!(
        run.errors.is_empty(),
        "`fmt --check` must not exit 2 over compiled artifacts"
    );
    assert_eq!(run.results.len(), 1);
    assert!(!run.results[0].changed, "a skipped file is not drift");
    assert_eq!(run.results[0].skipped.as_deref(), Some("binary file"));
    assert_eq!(
        std::fs::read(&path).expect("read fixture"),
        COMPILED_CATALOG,
        "the bytes are untouched"
    );
}

#[test]
fn malformed_text_without_nul_bytes_still_errors() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let path = directory.path().join("bad.py");
    std::fs::write(&path, MALFORMED_TEXT).expect("write malformed text");

    let format = poly_core::format_run(
        &[directory.path().to_path_buf()],
        &Config::default(),
        &options(),
        true,
        false,
    )
    .expect("format run completes");
    assert_eq!(
        format.errors.len(),
        1,
        "genuinely corrupt text is still a per-file error, not a binary skip"
    );

    let lint = poly_core::lint_run(
        &[directory.path().to_path_buf()],
        &Config::default(),
        &options(),
        false,
        false,
    )
    .expect("lint run completes");
    assert_eq!(
        lint.results[0].diagnostics.len(),
        1,
        "the invalid-utf8 diagnostic must survive the binary carve-out"
    );
    assert_eq!(lint.results[0].diagnostics[0].code.as_deref(), Some("invalid-utf8"));
}
