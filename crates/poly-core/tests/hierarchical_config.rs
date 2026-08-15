//! End-to-end tests for hierarchical (monorepo) config resolution (ADR 0018):
//! a nested `poly.toml` cascades over the root and governs only its own subtree,
//! while a single-root repo behaves exactly as before.

use std::fs;
use std::path::Path;

use poly_core::{Config, RunOptions};
use tempfile::tempdir;

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn options(force_exclude: bool) -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude,
        fix_generated: false,
        explicit_config: false,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
    }
}

/// Source that ruff will reformat, so "was it rewritten?" is a byte comparison
/// rather than a judgement call.
const UNFORMATTED: &str = "x = {  'a':1 }\n";

/// A monorepo whose nested project excludes its generated files, plus a
/// counterweight file in the same project that nothing excludes.
///
/// Returns the repo root. Layout:
///
/// ```text
/// poly.toml                    [workspace] root = true   (no exclude)
/// packages/py/poly.toml        [discovery] exclude = ["**/*_pb2.py"]
/// packages/py/src/wire_pb2.py  generated  — must never be rewritten
/// packages/py/src/app.py       hand-written — must always be rewritten
/// ```
fn generated_exclude_repo() -> tempfile::TempDir {
    let repo = tempdir().unwrap();
    let root = repo.path();
    write(&root.join("poly.toml"), "[workspace]\nroot = true\n");
    write(
        &root.join("packages/py/poly.toml"),
        "[discovery]\nexclude = [\"**/*_pb2.py\"]\n",
    );
    write(&root.join("packages/py/src/wire_pb2.py"), UNFORMATTED);
    write(&root.join("packages/py/src/app.py"), UNFORMATTED);
    repo
}

/// Assert the nested project's exclude held *and* that it did not hold by
/// excluding everything: the generated file is byte-identical, the hand-written
/// sibling was reformatted.
fn assert_generated_kept_and_sibling_formatted(root: &Path, context: &str) {
    let generated = fs::read_to_string(root.join("packages/py/src/wire_pb2.py")).unwrap();
    let sibling = fs::read_to_string(root.join("packages/py/src/app.py")).unwrap();
    assert_eq!(
        generated, UNFORMATTED,
        "{context}: the nested [discovery] exclude must keep wire_pb2.py untouched"
    );
    assert_ne!(
        sibling, UNFORMATTED,
        "{context}: counterweight — app.py is not excluded and must still be formatted"
    );
}

/// A nested `[discovery] exclude` must hold when the file is named *explicitly*,
/// not only when it is reached by a directory walk.
///
/// This is the hook path: `poly hooks` always passes explicit staged paths
/// (`poly lint --no-workspace --force-exclude <files>`), so a nested config that
/// excludes generated output was honored by `poly fmt .` and silently inert at
/// commit time — the generated file got rewritten anyway.
#[test]
fn nested_exclude_applies_to_an_explicitly_named_file() {
    let repo = generated_exclude_repo();
    let root = repo.path();
    let config = Config::load(root).expect("load root config");
    let paths = vec![
        root.join("packages/py/src/wire_pb2.py"),
        root.join("packages/py/src/app.py"),
    ];

    poly_core::format(&paths, &config, &options(true), true, false).unwrap();

    assert_generated_kept_and_sibling_formatted(root, "explicit file roots");
}

/// The same exclude must hold when the run root *is* the nested config's own
/// directory — `poly fmt packages/py` from the repo root. The config file sits
/// at the walk root, which is precisely where it used to be shadowed by the
/// run's fallback config.
#[test]
fn nested_exclude_applies_when_the_run_root_is_the_config_directory() {
    let repo = generated_exclude_repo();
    let root = repo.path();
    let config = Config::load(root).expect("load root config");

    poly_core::format(&[root.join("packages/py")], &config, &options(true), true, false).unwrap();

    assert_generated_kept_and_sibling_formatted(root, "run root == nested config dir");
}

/// And when the run root is *below* the nested config's directory.
#[test]
fn nested_exclude_applies_when_the_run_root_is_below_the_config_directory() {
    let repo = generated_exclude_repo();
    let root = repo.path();
    let config = Config::load(root).expect("load root config");

    poly_core::format(&[root.join("packages/py/src")], &config, &options(true), true, false).unwrap();

    assert_generated_kept_and_sibling_formatted(root, "run root below nested config dir");
}

/// A **directory-shaped** exclude must still reach a named file nested several
/// levels inside it.
///
/// A named file has no walk to prune, so it is matched in one shot against a
/// single frame — and the frame has to be wide enough to still contain the
/// directory the glob names. Anchoring it at the file's own directory looks
/// natural and silently breaks every `some_dir/**` rule for anything deeper than
/// `some_dir/file`: the frame has already consumed the component the glob is
/// about. The counterweights here are a file the exclude does not name and a
/// shallow match, so over-excluding cannot pass either.
#[test]
fn a_directory_exclude_reaches_a_deeply_nested_named_file() {
    let repo = tempdir().unwrap();
    let root = repo.path();
    write(
        &root.join("poly.toml"),
        "[workspace]\nroot = true\n[discovery]\nexclude = [\"generated/**\"]\n",
    );
    write(&root.join("generated/shallow.py"), UNFORMATTED);
    write(&root.join("generated/a/b/deep.py"), UNFORMATTED);
    write(&root.join("app.py"), UNFORMATTED);

    let config = Config::load(root).expect("load root config");
    let paths = vec![
        root.join("generated/shallow.py"),
        root.join("generated/a/b/deep.py"),
        root.join("app.py"),
    ];
    poly_core::format(&paths, &config, &options(true), true, false).unwrap();

    let read = |relative: &str| fs::read_to_string(root.join(relative)).unwrap();
    assert_eq!(read("generated/shallow.py"), UNFORMATTED, "shallow match excluded");
    assert_eq!(
        read("generated/a/b/deep.py"),
        UNFORMATTED,
        "`generated/**` must still exclude a file three levels down when it is named directly"
    );
    assert_ne!(
        read("app.py"),
        UNFORMATTED,
        "counterweight — app.py is outside generated/ and must still be formatted"
    );
}

/// Exclusions are only half of it: a nested config's *rules* must govern an
/// explicitly named file too, or the hook and the whole-repo run disagree about
/// what is a finding. The counterweight is the root file, named just as
/// explicitly, whose `F401` must still fire.
#[test]
fn nested_per_file_ignores_apply_to_an_explicitly_named_file() {
    let repo = tempdir().unwrap();
    let root = repo.path();
    write(
        &root.join("poly.toml"),
        "[workspace]\nroot = true\n[lint.python.ruff]\nselect = [\"F\"]\n",
    );
    write(&root.join("app.py"), "import os\n");
    write(
        &root.join("sub/poly.toml"),
        "[per-file-ignores]\n\"*.py\" = [\"F401\"]\n",
    );
    write(&root.join("sub/app.py"), "import os\n");

    let config = Config::load(root).expect("load root config");
    let paths = vec![root.join("sub/app.py"), root.join("app.py")];
    let results = poly_core::lint(&paths, &config, &options(true), false, false).unwrap();

    let fires = |path: &Path| {
        results
            .iter()
            .find(|r| r.path == path)
            .is_some_and(|r| r.diagnostics.iter().any(|d| d.code.as_deref() == Some("F401")))
    };
    assert!(
        !fires(&root.join("sub/app.py")),
        "nested [per-file-ignores] must suppress F401 for an explicitly named file"
    );
    assert!(
        fires(&root.join("app.py")),
        "counterweight — the root file has no ignore and must still report F401"
    );
}

/// A nested `[defaults]` must govern an explicitly named file as well, so the
/// hook path and `poly fmt .` produce the same bytes. Formatting the same source
/// through both entry points must converge.
#[test]
fn nested_defaults_apply_to_an_explicitly_named_file() {
    let source = "result = some_function_name(alpha_value, beta_value, gamma_value, delta)\n";
    let repo = tempdir().unwrap();
    let root = repo.path();
    write(
        &root.join("poly.toml"),
        "[workspace]\nroot = true\n[defaults]\nline_length = 120\n",
    );
    write(&root.join("sub/poly.toml"), "[defaults]\nline_length = 50\n");

    let file = root.join("sub/mod.py");
    write(&file, source);
    poly_core::format(&[root.to_path_buf()], &config_of(root), &options(true), true, false).unwrap();
    let via_directory = fs::read_to_string(&file).unwrap();

    write(&file, source);
    poly_core::format(
        std::slice::from_ref(&file),
        &config_of(root),
        &options(true),
        true,
        false,
    )
    .unwrap();
    let via_named_file = fs::read_to_string(&file).unwrap();

    assert_ne!(via_directory, source, "the nested line_length must wrap this call");
    assert_eq!(
        via_named_file, via_directory,
        "naming the file must apply the same nested [defaults] as walking into it"
    );
}

fn config_of(root: &Path) -> Config {
    Config::load(root).expect("load root config")
}

/// Nested (monorepo) run: the root config selects ruff's `F` family so an unused
/// import fires `F401`; a nested `poly.toml` under `sub/` adds a
/// `[per-file-ignores]` for `*.py`. The nested suppression must apply ONLY to
/// files under `sub/` — the root file still reports `F401`, proving per-file
/// config association and the per-config per-file-ignores path.
#[test]
fn nested_per_file_ignores_apply_only_to_their_subtree() {
    let repo = tempdir().unwrap();
    let root = repo.path();
    write(
        &root.join("poly.toml"),
        "[workspace]\nroot = true\n[lint.python.ruff]\nselect = [\"F\"]\n",
    );
    write(&root.join("app.py"), "import os\n");
    write(
        &root.join("sub/poly.toml"),
        "[per-file-ignores]\n\"*.py\" = [\"F401\"]\n",
    );
    write(&root.join("sub/app.py"), "import os\n");

    let config = Config::load(root).expect("load root config");
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        explicit_config: false,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
    };
    let results = poly_core::lint(&[root.to_path_buf()], &config, &opts, false, false).unwrap();

    let root_app = root.join("app.py");
    let sub_app = root.join("sub/app.py");

    let root_fires = results
        .iter()
        .find(|r| r.path == root_app)
        .is_some_and(|r| r.diagnostics.iter().any(|d| d.code.as_deref() == Some("F401")));
    assert!(
        root_fires,
        "root/app.py must still report F401 (root config has no ignore)"
    );

    let sub_reports = results.iter().any(|r| r.path == sub_app && !r.diagnostics.is_empty());
    assert!(
        !sub_reports,
        "sub/app.py F401 must be suppressed by the nested [per-file-ignores]"
    );
}

/// A nested config's `[defaults]` cascade: the child inherits the root's ruff
/// selection (so the same rule is computed) and overrides only what it declares.
/// Here the nested config raises the same suppression via inheritance of
/// `select` from the root — asserting the cascade base is read from disk.
#[test]
fn single_root_repo_reports_unsuppressed_diagnostic() {
    let repo = tempdir().unwrap();
    let root = repo.path();
    write(
        &root.join("poly.toml"),
        "[workspace]\nroot = true\n[lint.python.ruff]\nselect = [\"F\"]\n",
    );
    write(&root.join("sub/app.py"), "import os\n");

    let config = Config::load(root).expect("load root config");
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        explicit_config: false,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
    };
    let results = poly_core::lint(&[root.to_path_buf()], &config, &opts, false, false).unwrap();

    let sub_app = root.join("sub/app.py");
    let fires = results
        .iter()
        .find(|r| r.path == sub_app)
        .is_some_and(|r| r.diagnostics.iter().any(|d| d.code.as_deref() == Some("F401")));
    assert!(fires, "with no nested config, sub/app.py reports F401 (back-compat)");
}
