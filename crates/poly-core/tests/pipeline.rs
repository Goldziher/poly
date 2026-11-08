//! End-to-end pipeline tests for the core: discovery → routing → run → cache.
//!
//! The format fixtures are Lua, a language only the tree-sitter tier serves, so they need no component.
#![cfg(feature = "all-engines")]

use std::fs;

use poly_core::{Config, RunOptions};

fn write(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, content).unwrap();
    p
}

#[test]
fn lint_does_not_flag_trailing_whitespace() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.go", "package main   \nfunc main() {}\n");
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        generated: None,
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
        only: Vec::new(),
        skip: Vec::new(),
        ..RunOptions::default()
    };
    let results = poly_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();
    assert!(
        results.iter().all(|r| r.diagnostics.is_empty()),
        "trailing whitespace must not surface as a lint diagnostic, got {:?}",
        results.iter().flat_map(|r| &r.diagnostics).collect::<Vec<_>>()
    );
}

#[test]
fn format_check_does_not_write_but_reports_change() {
    let dir = tempfile::tempdir().unwrap();
    let messy = "x = 1   \n\n\n";
    let path = write(dir.path(), "a.lua", messy);
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        generated: None,
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
        only: Vec::new(),
        skip: Vec::new(),
        ..RunOptions::default()
    };

    let results = poly_core::format(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].changed, "check mode should detect a change");
    assert_eq!(fs::read_to_string(&path).unwrap(), messy);
}

#[test]
fn format_write_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "a.lua", "key = value   \n\n\n");
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        generated: None,
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
        only: Vec::new(),
        skip: Vec::new(),
        ..RunOptions::default()
    };

    let first = poly_core::format(&[dir.path().to_path_buf()], &cfg, &opts, true, false).unwrap();
    assert!(first[0].changed);
    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(after, "key = value\n", "trailing ws + blank lines normalized");

    let second = poly_core::format(&[dir.path().to_path_buf()], &cfg, &opts, true, false).unwrap();
    assert!(!second[0].changed, "formatting must be idempotent");
}

#[cfg(unix)]
#[test]
fn format_write_preserves_the_executable_bit() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "script.lua", "key = value   \n\n\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        generated: None,
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
        only: Vec::new(),
        skip: Vec::new(),
        ..RunOptions::default()
    };

    let results = poly_core::format(&[dir.path().to_path_buf()], &cfg, &opts, true, false).unwrap();
    assert!(
        results[0].changed,
        "the fixture must actually be rewritten for this to prove anything"
    );
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o755,
        "the atomic rewrite must carry the original mode across the rename"
    );
}

#[test]
fn cache_round_trips() {
    use poly_cache::{Namespace, ResultCache};
    let dir = tempfile::tempdir().unwrap();
    let cache = ResultCache::open(dir.path().join("cache"), true).unwrap();
    let opts = toml::Table::new();
    let digest = ResultCache::single_file_digest("some content");
    let key = ResultCache::key(Namespace::Fmt, "test", "1", &opts, &digest);
    cache.put(Namespace::Fmt, &key, b"formatted").unwrap();
    assert_eq!(cache.get(Namespace::Fmt, &key).as_deref(), Some(&b"formatted"[..]));
    let key2 = ResultCache::key(Namespace::Fmt, "test", "2", &opts, &digest);
    assert_ne!(key, key2);
}

/// A directory walk prunes excluded paths, but a file named on the command line
/// was always kept. That is right when a human names a file and wrong in a hook,
/// which is *always* handed explicit staged paths — so a repo excluding
/// `**/*.tf` found the exclude silently inert in its pre-commit hook, and poly
/// reformatted Terraform that `terraform fmt` then rejected.
#[test]
fn force_exclude_applies_the_exclude_set_to_explicitly_named_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("poly.toml"), "[discovery]\nexclude = [\"**/*.tf\"]\n").expect("write config");
    std::fs::create_dir_all(root.join("terraform")).expect("mkdir");
    let excluded = root.join("terraform/main.tf");
    std::fs::write(&excluded, "variable   \"a\"   {}\n").expect("write tf");
    let kept = root.join("app.py");
    std::fs::write(&kept, "x   =    1\n").expect("write py");

    let config = poly_core::Config {
        exclude: vec!["**/*.tf".to_string()],
        ..poly_core::Config::default()
    };
    let run = |paths: &[std::path::PathBuf], force: bool| {
        let opts = RunOptions {
            no_cache: true,
            force_exclude: force,
            ..RunOptions::default()
        };
        poly_core::format(paths, &config, &opts, false, false).expect("format")
    };

    // Naming the file explicitly checks it by default — that behaviour stands.
    assert_eq!(run(std::slice::from_ref(&excluded), false).len(), 1);

    // Under force_exclude the same explicit path is excluded...
    assert!(run(std::slice::from_ref(&excluded), true).is_empty());

    // ...while a path the exclude set does not match is still checked, so this
    // cannot be mistaken for "force_exclude drops everything".
    assert_eq!(run(std::slice::from_ref(&kept), true).len(), 1);
}

/// `lint --fix` asks the **same** question `fmt` does: it withholds a rewrite
/// only from a file whose header stamps a content hash over its body, not from
/// every file carrying a `DO NOT EDIT` banner.
///
/// The two used to disagree — `fmt` skipped on the hash stamp, `lint --fix`
/// withheld on any generated marker — so a banner-only generated file was
/// reformatted by `poly fmt` and, in the same repository, denied a one-character
/// lint autofix. Nothing about a banner justifies that split: it announces
/// provenance, not a checksum, so rewriting the file breaks no verify step.
///
/// The hash stamp is a different claim and the guard stays: reformatting a
/// stamped body invalidates the hash, the generator's verify step reports drift
/// on a file no human touched, and the remedy is a regen that discards the fix —
/// a loop. That is a correctness guard, not a noise preference.
#[test]
fn fix_is_withheld_only_from_hash_stamped_files_and_diagnostics_are_still_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    // `import asyncio` is unused, which ruff reports with an autofix — the
    // rewrite this test is about. `result` additionally reproduces the reported
    // F841, which carries no autofix by default.
    let body = "import asyncio\n\n\nasync def test_extract(inputs):\n    result = await extract_batch(inputs, None)\n";
    let banner = dir.path().join("banner.py");
    std::fs::write(
        &banner,
        format!("# This file is auto-generated by alef — DO NOT EDIT.\n{body}"),
    )
    .expect("write banner");
    let stamped = dir.path().join("stamped.py");
    std::fs::write(
        &stamped,
        format!(
            "# This file is auto-generated by alef — DO NOT EDIT.\n\
             # alef:hash:a3f1c2d4e5b6a7980123456789abcdef0123456789abcdef0123456789abcdef\n{body}"
        ),
    )
    .expect("write stamped");
    let handwritten = dir.path().join("handwritten.py");
    std::fs::write(&handwritten, body).expect("write handwritten");

    let fix = |path: &std::path::Path, fix_generated: bool| {
        let before = std::fs::read_to_string(path).expect("read");
        let opts = RunOptions {
            no_cache: true,
            fix_generated,
            ..RunOptions::default()
        };
        let results = poly_core::lint(
            std::slice::from_ref(&path.to_path_buf()),
            &Config::default(),
            &opts,
            true,
            false,
        )
        .expect("lint");
        let after = std::fs::read_to_string(path).expect("read");
        (results, before != after)
    };

    // The new default: a banner is provenance, not a checksum, so the fix lands
    // — exactly as `poly fmt` already reformatted the same file.
    let (results, rewritten) = fix(&banner, false);
    assert!(rewritten, "a banner-only generated file must be fixed by --fix");
    assert!(
        !results.iter().any(|r| r.fix_withheld_generated),
        "nothing was withheld, so the reporter must not say it was"
    );

    // The hash-stamp guard survives, and reports rather than silently passing.
    let (results, rewritten) = fix(&stamped, false);
    assert!(!rewritten, "a hash-stamped file must not be rewritten by --fix");
    assert!(!results.is_empty(), "diagnostics must still be reported");
    assert!(
        results.iter().any(|r| r.fix_withheld_generated),
        "the withheld fix must be visible to the reporter, not silent"
    );

    // A handwritten file with the same defect is still fixed, so this cannot be
    // mistaken for "--fix stopped working".
    let (_, rewritten) = fix(&handwritten, false);
    assert!(rewritten, "handwritten file must still be fixed");

    // And the escape hatch opts back in.
    let (_, rewritten) = fix(&stamped, true);
    assert!(rewritten, "--fix-generated must apply fixes to hash-stamped files");
}

/// `fmt` skipping a file removes it from the format gate entirely, so it must
/// require a *content hash* — not a bare "DO NOT EDIT" banner.
///
/// Generalising this caused real harm: a generator stamping a hand-written file
/// with a banner silently dropped that file out of lint and format enforcement,
/// and a consumer's most user-facing code left the gate with nothing reporting
/// it. Reformatting a banner-only file is harmless; not checking it is not.
#[test]
fn fmt_skips_hash_stamped_files_but_not_banner_only_ones() {
    let dir = tempfile::tempdir().expect("tempdir");
    let banner = dir.path().join("banner_only.py");
    std::fs::write(&banner, "# This file is managed by alef — DO NOT EDIT.\nx   =    1\n").expect("write");
    let hashed = dir.path().join("hashed.py");
    std::fs::write(
        &hashed,
        "# Auto-generated — DO NOT EDIT.\n# alef:hash: a3f1c2d4e5b6a7980123456789abcdef0123456789abcdef0123456789abcdef\ny   =    2\n",
    )
    .expect("write");

    let check = |path: &std::path::Path| {
        let opts = RunOptions {
            no_cache: true,
            ..RunOptions::default()
        };
        poly_core::format(
            std::slice::from_ref(&path.to_path_buf()),
            &Config::default(),
            &opts,
            false,
            false,
        )
        .expect("format")
    };

    // Banner only: still checked, and its drift is still reported.
    let results = check(&banner);
    assert!(
        results.iter().any(|r| r.changed),
        "a banner-only file must stay in the format gate"
    );
    assert!(results.iter().all(|r| r.skipped.is_none()));

    // Hash-stamped: skipped, because reformatting would invalidate the hash.
    let results = check(&hashed);
    assert!(results.iter().all(|r| !r.changed));
    assert!(
        results.iter().any(|r| r.skipped.is_some()),
        "a hash-stamped file must be skipped and reported as skipped"
    );
}

/// A generator accepts its banner anywhere in its first ten lines and stamps the
/// hash directly beneath it, so a licence preamble puts the stamp below the
/// banner window. poly scanned only five lines, so these files were stamped by
/// their generator, unprotected here, and reformatted by `fmt --fix` — the
/// generator's verify step then reported drift on a file no human had touched.
///
/// The two arms differ by the preamble alone. Both carry the same real stamp, so
/// a regression that re-narrows the window fails the first arm while the second
/// keeps passing, which is what makes this falsifiable rather than decorative.
#[test]
fn fmt_skips_hash_stamped_files_whose_stamp_sits_below_a_licence_preamble() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stamped_body = |preamble: usize| {
        format!(
            "{}# This file is auto-generated by alef — DO NOT EDIT.\n\
             # alef:hash:a3f1c2d4e5b6a7980123456789abcdef0123456789abcdef0123456789abcdef\n\
             # To regenerate: alef generate\n\
             z   =    3\n",
            "# Copyright 2026 xberg-io\n".repeat(preamble)
        )
    };

    // `write: true` — the `fmt --fix` path consumers actually run. In check mode
    // nothing is written, so the byte comparison below would pass even if the
    // skip were removed entirely.
    let format_once = |path: &std::path::Path| {
        let options = RunOptions {
            no_cache: true,
            ..RunOptions::default()
        };
        poly_core::format(
            std::slice::from_ref(&path.to_path_buf()),
            &Config::default(),
            &options,
            true,
            false,
        )
        .expect("format")
    };

    // Control: no preamble, stamp on line 1 — protected before this fix too.
    let no_preamble = write(dir.path(), "no_preamble.py", &stamped_body(0));
    let results = format_once(&no_preamble);
    assert!(results.iter().all(|r| !r.changed));
    assert!(results.iter().any(|r| r.skipped.is_some()));

    // The regression: four preamble lines push the stamp to line 5.
    let with_preamble = write(dir.path(), "with_preamble.py", &stamped_body(4));
    let before = std::fs::read(&with_preamble).expect("read before");
    let results = format_once(&with_preamble);
    assert!(
        results.iter().any(|r| r.skipped.is_some()),
        "a stamped file must stay skipped when a preamble precedes its banner"
    );
    assert!(results.iter().all(|r| !r.changed));
    assert_eq!(
        std::fs::read(&with_preamble).expect("read after"),
        before,
        "the stamped file must be byte-for-byte unchanged"
    );
}

#[test]
fn fmt_checks_explicit_rust_hash_module_import() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = write(dir.path(), "hash_import.rs", "use a::hash::b;\n");
    let options = RunOptions {
        no_cache: true,
        ..RunOptions::default()
    };

    let results = poly_core::format(
        std::slice::from_ref(&source),
        &Config::default(),
        &options,
        false,
        false,
    )
    .expect("format explicit Rust source");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].path, source);
    assert_eq!(results[0].skipped, None);
    assert_eq!(results[0].error, None);
}

#[test]
fn fmt_skips_hash_stamp_below_yaml_frontmatter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = write(
        dir.path(),
        "SKILL.md",
        "---\nname: api\ndescription: generated API docs\n---\n\n\
         <!-- This file is auto-generated by alef. DO NOT EDIT. -->\n\
         <!-- alef:hash:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef -->\n\
         #Heading\n",
    );
    let options = RunOptions {
        no_cache: true,
        ..RunOptions::default()
    };

    let results = poly_core::format(
        std::slice::from_ref(&source),
        &Config::default(),
        &options,
        false,
        false,
    )
    .expect("format frontmatter source");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].path, source);
    assert!(!results[0].changed);
    assert_eq!(
        results[0].skipped.as_deref(),
        Some("hash-stamped generated file (pass --fix-generated to format)")
    );
}

/// A path named on the command line is a request to check that path, so one that
/// no engine covers has to be named back. The mixed invocation is the case that
/// matters: reconciling one explicit argument against a whole discovered corpus
/// must give the same answer as reconciling many.
#[test]
fn lint_names_unmatched_explicit_paths_alongside_a_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "app.py", "x = 1\n");
    // Enough arguments to cross the threshold where the reconciliation switches
    // from scanning the discovered set to indexing it — both paths must agree.
    let unmatched: Vec<std::path::PathBuf> = (0..9)
        .map(|i| {
            write(
                dir.path(),
                &format!("notes{i}.unknownext"),
                "not a language poly knows\n",
            )
        })
        .collect();

    let opts = RunOptions {
        no_cache: true,
        explicit_config: true,
        ..RunOptions::default()
    };
    let skipped_for = |paths: &[std::path::PathBuf]| -> Vec<std::path::PathBuf> {
        let mut paths: Vec<std::path::PathBuf> = poly_core::lint_run(paths, &Config::default(), &opts, false, false)
            .expect("lint run")
            .skipped
            .into_iter()
            .filter(|s| s.reason == poly_core::runner::NO_ENGINE_SKIP)
            .map(|s| s.path)
            .collect();
        paths.sort();
        paths
    };

    // A directory walk alone narrates nothing: what a walk does not match is not
    // a skip.
    assert_eq!(
        skipped_for(&[dir.path().to_path_buf()]),
        Vec::<std::path::PathBuf>::new()
    );

    // One explicit path plus a directory — the scanned branch.
    assert_eq!(
        skipped_for(&[unmatched[0].clone(), dir.path().to_path_buf()]),
        vec![unmatched[0].clone()],
        "a mixed invocation must still name the unmatched explicit path"
    );

    // Nine explicit paths plus a directory — the indexed branch, same answer.
    let mut mixed = unmatched.clone();
    mixed.push(dir.path().to_path_buf());
    let mut expected = unmatched.clone();
    expected.sort();
    assert_eq!(skipped_for(&mixed), expected);
}

#[test]
fn lint_orders_a_files_diagnostics_identically_on_every_run() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (0..12).map(|i| format!("// poly: allow[X{i}]\n")).collect();
    write(dir.path(), "a.go", &format!("package main\n\n{body}func main() {{}}\n"));
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(4),
        explicit_config: true,
        ..RunOptions::default()
    };
    let run = || {
        let results = poly_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();
        results
            .iter()
            .flat_map(|r| &r.diagnostics)
            .map(|d| {
                (
                    d.span.as_ref().map(|s| (s.start_line, s.start_col)),
                    d.code.clone(),
                    d.title.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let first = run();
    assert!(first.len() > 6, "the fixture must produce findings, got {first:?}");
    let mut sorted = first.clone();
    sorted.sort();
    assert_eq!(first, sorted, "findings must come out ordered by location");
    for _ in 0..5 {
        assert_eq!(run(), first);
    }
}
