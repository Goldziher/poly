use poly_core::{Config, RunOptions, Severity};

const INVALID_UTF8: &[u8] = b"x = 1\n\xff\xfe not utf-8\n";

#[test]
fn invalid_utf8_is_a_contextual_error_without_aborting_the_run() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let path = directory.path().join("bad.py");
    std::fs::write(&path, INVALID_UTF8).expect("write invalid UTF-8 fixture");

    let run = poly_core::lint_run(
        &[directory.path().to_path_buf()],
        &Config::default(),
        &RunOptions {
            no_cache: true,
            jobs: Some(1),
            exclude: Vec::new(),
            force_exclude: false,
            fix_generated: false,
            explicit_config: true,
            config_resolver: None,
            externally_linted_languages: Vec::new(),
        },
        false,
        false,
    )
    .expect("lint run continues past invalid UTF-8");

    assert!(
        run.errors.is_empty(),
        "invalid UTF-8 is a per-file diagnostic, not a fatal run error"
    );
    assert_eq!(run.checked, 0, "a file whose text could not be decoded was not linted");
    assert_eq!(run.results.len(), 1, "the error remains visible in structured output");
    assert_eq!(
        run.skipped.len(),
        1,
        "the file is explicitly accounted for as uninspected"
    );

    let result = &run.results[0];
    assert_eq!(result.path, path);
    assert!(result.error.is_none());
    assert!(result.skipped.as_deref().is_some_and(|reason| reason.contains("UTF-8")));
    assert_eq!(result.diagnostics.len(), 1);

    let diagnostic = &result.diagnostics[0];
    assert_eq!(diagnostic.engine, "poly");
    assert_eq!(diagnostic.code.as_deref(), Some("invalid-utf8"));
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(
        diagnostic.title.contains("byte 6"),
        "diagnostic should locate the invalid sequence"
    );
    assert!(
        diagnostic
            .description
            .as_deref()
            .is_some_and(|description| description.contains("text linting was skipped")),
        "diagnostic should explain the consequence"
    );
}

#[test]
fn invalid_utf8_format_is_a_contextual_per_file_error_without_mutation() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let path = directory.path().join("bad.py");
    std::fs::write(&path, INVALID_UTF8).expect("write invalid UTF-8 fixture");

    let run = poly_core::format_run(
        &[directory.path().to_path_buf()],
        &Config::default(),
        &RunOptions {
            no_cache: true,
            jobs: Some(1),
            exclude: Vec::new(),
            force_exclude: false,
            fix_generated: false,
            explicit_config: true,
            config_resolver: None,
            externally_linted_languages: Vec::new(),
        },
        true,
        false,
    )
    .expect("format run continues past invalid UTF-8");

    assert_eq!(run.errors.len(), 1, "the decode failure remains a per-file error");
    assert!(run.results.is_empty(), "the file was not formatted");
    assert_eq!(run.errors[0].path, path);
    assert!(run.errors[0].message.contains("not valid UTF-8 at byte 6"));
    assert!(run.errors[0].message.contains("text formatting was skipped"));
    assert_eq!(
        std::fs::read(&path).expect("read fixture after format"),
        INVALID_UTF8,
        "format --fix must not mutate undecodable bytes"
    );
}
