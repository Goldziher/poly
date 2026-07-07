//! Unit tests for the `poly-config` schema and cascade resolution.
//! Extracted from `lib.rs` to keep that file under the 1000-line module cap.

use std::fs;

use tempfile::tempdir;

use super::*;

#[test]
fn default_when_no_file_present() {
    let dir = tempdir().unwrap();
    let config = PolyConfig::load(dir.path()).expect("load");
    assert_eq!(config.defaults.line_length, 120);
    assert!(config.lint.is_empty());
    assert!(config.hooks.stage_configs.is_empty());
    assert!(!config.hooks.builtin.lint.enabled);
}

#[test]
fn parses_defaults_lint_and_fmt() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(
        &path,
        r#"
[defaults]
line_length = 100
line_ending = "crlf"

[lint.python.ruff]
select = ["E", "F"]

[fmt.javascript.oxc]
semicolons = true
"#,
    )
    .unwrap();
    let config = PolyConfig::load_file(&path).expect("load");
    assert_eq!(config.defaults.line_length, 100);
    assert_eq!(config.defaults.line_ending, LineEnding::Crlf);
    assert!(config.lint.contains_key("python"));
    assert!(config.fmt.contains_key("javascript"));
}

#[test]
fn parses_discovery_exclude() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(
        &path,
        r#"
[discovery]
exclude = ["test_apps/**", "artifacts/**"]
"#,
    )
    .unwrap();
    let config = PolyConfig::load_file(&path).expect("load");
    assert_eq!(
        config.discovery.exclude.as_slice(),
        &["test_apps/**".to_string(), "artifacts/**".to_string()],
    );
}

#[test]
fn parses_per_file_ignores() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(
        &path,
        "[per-file-ignores]\n\"tests/**\" = [\"F401\", \"too-many-methods\"]\n\"*.gen.py\" = [\"E501\"]\n",
    )
    .unwrap();
    let config = PolyConfig::load_file(&path).expect("load");
    assert_eq!(
        config.per_file_ignores.get("tests/**"),
        Some(&vec!["F401".to_string(), "too-many-methods".to_string()]),
    );
    assert_eq!(config.per_file_ignores.get("*.gen.py"), Some(&vec!["E501".to_string()]),);
}

#[test]
fn discovery_exclude_accepts_a_single_string() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(&path, "[discovery]\nexclude = \"test_apps/**\"\n").unwrap();
    let config = PolyConfig::load_file(&path).expect("load");
    assert_eq!(config.discovery.exclude.as_slice(), &["test_apps/**".to_string()]);
}

#[test]
fn absent_discovery_table_yields_no_excludes() {
    let dir = tempdir().unwrap();
    let config = PolyConfig::load(dir.path()).expect("load");
    assert!(config.discovery.exclude.is_empty());
}

#[test]
fn legacy_polylint_toml_is_not_discovered() {
    // Clean break (v0.9): only `poly.toml` is read. A lone `polylint.toml` is
    // ignored, so the default config applies.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("polylint.toml"), "[defaults]\nline_length = 77\n").unwrap();
    let config = PolyConfig::load(dir.path()).expect("load");
    assert_eq!(
        config.defaults.line_length, 120,
        "polylint.toml must be ignored; default line_length applies"
    );
}

#[test]
fn parses_commit_section() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(
        &path,
        r#"
[commit]
preset = "conventional"
[commit.rules]
require_body = true
no_emojis = true
[[commit.rules.excludes]]
pattern = "^WIP"
"#,
    )
    .unwrap();
    let config = PolyConfig::load_file(&path).expect("load");
    assert_eq!(config.commit.preset.as_deref(), Some("conventional"));
    assert_eq!(config.commit.rules.require_body, Some(true));
    assert_eq!(config.commit.rules.excludes.len(), 1);
    assert_eq!(config.commit.rules.excludes[0].pattern, "^WIP");
}

#[test]
fn absent_cache_table_yields_defaults() {
    let dir = tempdir().unwrap();
    let config = PolyConfig::load(dir.path()).expect("load");
    assert!(config.cache.enabled, "cache.enabled must default to true");
    assert_eq!(config.cache.results.hooks, crate::HookCacheMode::Safe);
    assert!(!config.cache.sccache.enabled, "sccache.enabled must default to false");
    assert!(config.cache.dir.is_none());
}

#[test]
fn parses_cache_table_full() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(
        &path,
        r#"
[cache]
enabled = true

[cache.results]
hooks = "safe"

[cache.sccache]
enabled = true
bin = "/usr/bin/sccache"
dir = "/tmp/sccache"
max_size = "5G"
"#,
    )
    .unwrap();
    let config = PolyConfig::load_file(&path).expect("load");
    assert!(config.cache.enabled);
    assert_eq!(config.cache.results.hooks, crate::HookCacheMode::Safe);
    assert!(config.cache.sccache.enabled);
    assert_eq!(config.cache.sccache.bin.as_deref(), Some("/usr/bin/sccache"));
    assert_eq!(config.cache.sccache.dir.as_deref(), Some("/tmp/sccache"));
    assert_eq!(config.cache.sccache.max_size.as_deref(), Some("5G"));
}

#[test]
fn parses_cache_mode_off_and_aggressive() {
    let dir = tempdir().unwrap();
    let off_path = dir.path().join("off.toml");
    fs::write(&off_path, "[cache.results]\nhooks = \"off\"\n").unwrap();
    let config_off = PolyConfig::load_file(&off_path).expect("load off");
    assert_eq!(config_off.cache.results.hooks, crate::HookCacheMode::Off);

    let agg_path = dir.path().join("agg.toml");
    fs::write(&agg_path, "[cache.results]\nhooks = \"aggressive\"\n").unwrap();
    let config_agg = PolyConfig::load_file(&agg_path).expect("load aggressive");
    assert_eq!(config_agg.cache.results.hooks, crate::HookCacheMode::Aggressive);
}

#[test]
fn parses_cache_disabled_with_dir_override() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(&path, "[cache]\nenabled = false\ndir = \"/custom/cache\"\n").unwrap();
    let config = PolyConfig::load_file(&path).expect("load");
    assert!(!config.cache.enabled);
    assert_eq!(config.cache.dir.as_deref(), Some("/custom/cache"));
}

#[test]
fn parses_hooks_builtin_and_inline_stages() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(
        &path,
        r#"
[hooks]
stages = ["pre-commit"]

[hooks.builtin]
lint = true
fmt = { stages = ["pre-commit"] }
commit = { enabled = false }

[hooks.pre-commit]
parallel = true
[[hooks.pre-commit.jobs]]
run = "cargo fmt --check"
"#,
    )
    .unwrap();
    let config = PolyConfig::load_file(&path).expect("load");
    assert_eq!(config.hooks.stages, vec!["pre-commit".to_string()]);
    // bare `true`
    assert!(config.hooks.builtin.lint.enabled);
    assert!(config.hooks.builtin.lint.stages.is_empty());
    // table without `enabled` → enabled
    assert!(config.hooks.builtin.fmt.enabled);
    assert_eq!(config.hooks.builtin.fmt.stages, vec!["pre-commit"]);
    // table with explicit `enabled = false`
    assert!(!config.hooks.builtin.commit.enabled);
    // inline stage
    let pre_commit = &config.hooks.stage_configs[&Stage::PreCommit];
    assert!(pre_commit.parallel);
    assert_eq!(pre_commit.jobs.len(), 1);
    assert_eq!(pre_commit.jobs[0].run.as_deref(), Some("cargo fmt --check"));
}

#[test]
fn imported_repos_are_rejected_at_load() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(
        &path,
        r#"
[[hooks.repo]]
repo = "https://github.com/example/hooks"
"#,
    )
    .unwrap();
    let error = PolyConfig::load_file(&path).unwrap_err().to_string();
    assert!(error.contains("no longer supported"), "{error}");
}

#[test]
fn invalid_hooks_job_fails_load() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(
        &path,
        r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
run = "x"
script = "y.sh"
"#,
    )
    .unwrap();
    let error = PolyConfig::load_file(&path).unwrap_err().to_string();
    assert!(error.contains("invalid [hooks] config"), "{error}");
}

#[test]
fn local_override_deep_merges_nested_value() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("poly.toml"),
        r#"
[defaults]
line_length = 100
[cache.results]
hooks = "safe"
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join(LOCAL_OVERRIDE_NAME),
        r#"
[defaults]
line_length = 80
"#,
    )
    .unwrap();
    let config = PolyConfig::load(dir.path()).expect("load");
    // Overridden nested scalar takes the local value...
    assert_eq!(config.defaults.line_length, 80);
    // ...while untouched nested tables are preserved from the base.
    assert_eq!(config.cache.results.hooks, crate::HookCacheMode::Safe);
}

#[test]
fn parses_tools_table_from_poly_toml() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(
        &path,
        r#"
[tools.shfmt]
enabled = true
args = ["-i", "2"]
stages = ["pre-commit"]

[tools.clang-format]
enabled = true
"#,
    )
    .unwrap();
    let config = PolyConfig::load_file(&path).expect("load");
    assert_eq!(config.tools.len(), 2);
    let shfmt = config.tools.get("shfmt").expect("shfmt present");
    assert!(shfmt.enabled);
    assert_eq!(shfmt.args.as_deref(), Some(&["-i".to_string(), "2".to_string()][..]));
    assert_eq!(shfmt.stages, vec![Stage::PreCommit]);
    assert!(config.tools.get("clang-format").unwrap().enabled);
}

#[test]
fn unknown_tool_name_fails_load() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(&path, "[tools.not-a-real-tool]\nenabled = true\n").unwrap();
    let error = PolyConfig::load_file(&path).unwrap_err().to_string();
    assert!(error.contains("invalid [tools] config"), "{error}");
    assert!(error.contains("not-a-real-tool"), "{error}");
}

#[test]
fn absent_tools_table_yields_empty() {
    let dir = tempdir().unwrap();
    let config = PolyConfig::load(dir.path()).expect("load");
    assert!(config.tools.is_empty());
}

#[test]
fn absent_local_override_is_a_no_op() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("poly.toml"), "[defaults]\nline_length = 99\n").unwrap();
    let config = PolyConfig::load(dir.path()).expect("load");
    assert_eq!(config.defaults.line_length, 99);
}

#[test]
fn resolve_for_dir_cascades_child_over_root() {
    let root = tempdir().unwrap();
    fs::write(
        root.path().join("poly.toml"),
        r#"
[workspace]
root = true
[defaults]
line_length = 120
[lint.python.ruff]
select = ["ALL"]
"#,
    )
    .unwrap();
    let child = root.path().join("frontend");
    fs::create_dir(&child).unwrap();
    // Child declares ONLY a diff; it must inherit line_length + ruff select.
    fs::write(child.join("poly.toml"), "[fmt.javascript.oxc]\nsemicolons = true\n").unwrap();

    let config = PolyConfig::resolve_for_dir(&child).expect("resolve");
    assert_eq!(config.defaults.line_length, 120, "inherited from root");
    assert!(config.lint.contains_key("python"), "ruff table inherited from root");
    assert!(config.fmt.contains_key("javascript"), "oxc table from child");
}

#[test]
fn resolve_for_dir_child_scalar_overrides_root() {
    let root = tempdir().unwrap();
    fs::write(
        root.path().join("poly.toml"),
        "[workspace]\nroot = true\n[defaults]\nline_length = 120\n",
    )
    .unwrap();
    let child = root.path().join("docs-site");
    fs::create_dir(&child).unwrap();
    fs::write(child.join("poly.toml"), "[defaults]\nline_length = 80\n").unwrap();

    let config = PolyConfig::resolve_for_dir(&child).expect("resolve");
    assert_eq!(config.defaults.line_length, 80, "nearest config wins");
}

#[test]
fn workspace_root_marker_bounds_the_chain() {
    // outer/poly.toml is ABOVE the marked root and must NOT be inherited.
    let outer = tempdir().unwrap();
    fs::write(outer.path().join("poly.toml"), "[defaults]\nline_length = 200\n").unwrap();
    let repo = outer.path().join("repo");
    fs::create_dir(&repo).unwrap();
    fs::write(
        repo.join("poly.toml"),
        "[workspace]\nroot = true\n[defaults]\nline_length = 120\n",
    )
    .unwrap();
    let pkg = repo.join("pkg");
    fs::create_dir(&pkg).unwrap();
    fs::write(pkg.join("poly.toml"), "[lint.rust.clippy]\n").unwrap();

    let config = PolyConfig::resolve_for_dir(&pkg).expect("resolve");
    assert_eq!(
        config.defaults.line_length, 120,
        "bounded at [workspace] root, not outer 200"
    );
}

#[test]
fn resolve_for_dir_single_config_matches_load() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("poly.toml"),
        "[workspace]\nroot = true\n[defaults]\nline_length = 111\n[lint.python.ruff]\nselect = [\"E\"]\n",
    )
    .unwrap();
    let resolved = PolyConfig::resolve_for_dir(dir.path()).expect("resolve");
    let loaded = PolyConfig::load(dir.path()).expect("load");
    assert_eq!(resolved.defaults.line_length, loaded.defaults.line_length);
    assert_eq!(
        resolved.lint, loaded.lint,
        "single-config resolve == load (back-compat)"
    );
}

#[test]
fn resolve_for_dir_no_config_is_default() {
    let dir = tempdir().unwrap();
    let config = PolyConfig::resolve_for_dir(dir.path()).expect("resolve");
    assert_eq!(config.defaults.line_length, 120);
    assert!(config.lint.is_empty());
}

#[test]
fn rules_dirs_resolve_against_config_root_not_cwd() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(&path, "[rules]\ndirs = [\"lint/rules\"]\n").unwrap();
    // Load from a nested subdir: the relative rule dir must anchor at the
    // config file's directory, not the (arbitrary) start directory.
    let nested = dir.path().join("a").join("b");
    fs::create_dir_all(&nested).unwrap();
    let config = PolyConfig::load(&nested).expect("load");
    assert_eq!(
        config.rules.dirs,
        vec![dir.path().join("lint/rules").to_string_lossy().into_owned()],
    );
}

#[test]
fn cascade_resolves_inherited_rules_dirs_against_declaring_config() {
    // Root declares `[rules] dirs`; the child config omits `[rules]`, so the
    // dirs are inherited. They must resolve against the ROOT (the config that
    // declared them), not the nearest (child) config.
    let root = tempdir().unwrap();
    fs::write(
        root.path().join("poly.toml"),
        "[workspace]\nroot = true\n[rules]\ndirs = [\".poly/rules\"]\n",
    )
    .unwrap();
    let child = root.path().join("packages").join("myapp");
    fs::create_dir_all(&child).unwrap();
    fs::write(child.join("poly.toml"), "[lint.python.ruff]\nselect = [\"E\"]\n").unwrap();

    let config = PolyConfig::resolve_for_dir(&child).expect("resolve");
    assert_eq!(
        config.rules.dirs,
        vec![root.path().join(".poly/rules").to_string_lossy().into_owned()],
        "inherited rule dirs must anchor at the root config that declared them",
    );
}

#[test]
fn cascade_child_rules_dirs_win_and_anchor_at_child() {
    // When the child DECLARES its own `[rules] dirs`, those win and resolve
    // against the child directory.
    let root = tempdir().unwrap();
    fs::write(
        root.path().join("poly.toml"),
        "[workspace]\nroot = true\n[rules]\ndirs = [\".poly/rules\"]\n",
    )
    .unwrap();
    let child = root.path().join("frontend");
    fs::create_dir(&child).unwrap();
    fs::write(child.join("poly.toml"), "[rules]\ndirs = [\"lint-rules\"]\n").unwrap();

    let config = PolyConfig::resolve_for_dir(&child).expect("resolve");
    assert_eq!(
        config.rules.dirs,
        vec![child.join("lint-rules").to_string_lossy().into_owned()],
    );
}

#[test]
fn rules_dirs_leave_absolute_paths_untouched() {
    // The resolver leaves absolute `dirs` entries verbatim and only anchors
    // relative ones — so the literal must be absolute on the *host* platform
    // (`/etc/...` is not absolute on Windows, where it would be anchored).
    #[cfg(unix)]
    let absolute = "/etc/poly/rules";
    #[cfg(windows)]
    let absolute = "C:/etc/poly/rules";

    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(&path, format!("[rules]\ndirs = [\"{absolute}\"]\n")).unwrap();
    let config = PolyConfig::load_file(&path).expect("load");
    assert_eq!(config.rules.dirs, vec![absolute.to_string()]);
}

#[test]
fn default_rules_dir_anchors_at_start_when_no_config() {
    let dir = tempdir().unwrap();
    let config = PolyConfig::load(dir.path()).expect("load");
    assert_eq!(
        config.rules.dirs,
        vec![dir.path().join(".poly/rules").to_string_lossy().into_owned()],
    );
}

#[test]
fn parses_workspace_root_marker() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(&path, "[workspace]\nroot = true\n").unwrap();
    let config = PolyConfig::load_file(&path).expect("load");
    assert!(config.workspace.root);
}

#[test]
fn parses_native_typos_ignore_regexes_and_maps() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("_typos.toml"),
        r#"
[default]
extend-ignore-re = ["0x[0-9a-f]+", "SPDX-.*"]
extend-ignore-words-re = ["^[A-Z]{2,}$"]
extend-ignore-identifiers-re = ["_impl$"]
[default.extend-words]
ba = "ba"
[default.extend-identifiers]
O_WRONLY = "O_WRONLY"
[files]
extend-exclude = ["*.lock"]
"#,
    )
    .unwrap();
    let config = PolyConfig::load(dir.path()).expect("load");
    let t = &config.typos_native;
    assert_eq!(
        t.extend_ignore_re,
        vec!["0x[0-9a-f]+".to_string(), "SPDX-.*".to_string()]
    );
    assert_eq!(t.extend_ignore_words_re, vec!["^[A-Z]{2,}$".to_string()]);
    assert_eq!(t.extend_ignore_identifiers_re, vec!["_impl$".to_string()]);
    assert_eq!(t.extend_words.get("ba"), Some(&"ba".to_string()));
    assert_eq!(t.extend_identifiers.get("O_WRONLY"), Some(&"O_WRONLY".to_string()));
    assert_eq!(t.extend_exclude, vec!["*.lock".to_string()]);
}

#[test]
fn merges_ancestor_typos_configs_unioning_regexes() {
    let root = tempdir().unwrap();
    fs::write(
        root.path().join("_typos.toml"),
        "[default]\nextend-ignore-re = [\"root-re\"]\n[default.extend-words]\nfoo = \"foo\"\n",
    )
    .unwrap();
    let sub = root.path().join("pkg");
    fs::create_dir(&sub).unwrap();
    fs::write(
        sub.join("_typos.toml"),
        "[default]\nextend-ignore-re = [\"sub-re\"]\n[default.extend-words]\nbar = \"bar\"\n",
    )
    .unwrap();

    let config = PolyConfig::load(&sub).expect("load");
    let t = &config.typos_native;
    // Regex lists union across the whole ancestor chain.
    assert!(t.extend_ignore_re.contains(&"root-re".to_string()), "{t:?}");
    assert!(t.extend_ignore_re.contains(&"sub-re".to_string()), "{t:?}");
    // Word maps merge from both directories.
    assert_eq!(t.extend_words.get("foo"), Some(&"foo".to_string()));
    assert_eq!(t.extend_words.get("bar"), Some(&"bar".to_string()));
}

#[test]
fn reads_pyproject_typos_and_codespell_sections() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("pyproject.toml"),
        r#"
[tool.typos.default]
extend-ignore-re = ["pyproj-re"]
[tool.typos.default.extend-words]
ba = "ba"
[tool.codespell]
ignore-words-list = "inh, te, tha"
"#,
    )
    .unwrap();
    let config = PolyConfig::load(dir.path()).expect("load");
    let t = &config.typos_native;
    assert_eq!(t.extend_ignore_re, vec!["pyproj-re".to_string()]);
    assert_eq!(t.extend_words.get("ba"), Some(&"ba".to_string()));
    for word in ["inh", "te", "tha"] {
        assert!(
            t.extend_ignore_words.contains(&word.to_string()),
            "codespell ignore-words-list should fold into extend_ignore_words: {t:?}",
        );
    }
}

#[test]
fn pyproject_without_typos_config_is_ignored() {
    let dir = tempdir().unwrap();
    // A manifest with no typos/codespell section must not be treated as a
    // typos source (and must not error).
    fs::write(
        dir.path().join("pyproject.toml"),
        "[project]\nname = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let config = PolyConfig::load(dir.path()).expect("load");
    assert!(config.typos_native.extend_words.is_empty());
    assert!(config.typos_native.extend_ignore_re.is_empty());
}
