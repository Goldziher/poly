//! skif as a project check: the findings of a known monorepo, the edits of its safe fixes, and the proof that
//! the cache key covers what skif read.
//!
//! The fixture is skif's own `fixtures/e2e/pnpm/dirty` workspace. It is driven through the engine with the files
//! loaded the way the host hands them over, then through the pipeline (`poly_core::lint_run`, cache on).
#![allow(unsafe_code, reason = "POLY_CACHE_HOME is set once, before any thread reads it")]

use std::fs;
use std::path::{Path, PathBuf};

use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::engine::{ProjectEnvironment, ProjectFile, ProjectRequest};
use poly_engine_api::language::Language;
use poly_engines_web::skif::{Analysis, analyze};

mod common;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skif/pnpm-dirty")
}

fn cfg(extra: &[(&str, toml::Value)]) -> EngineConfig {
    let mut options = toml::Table::new();
    options.insert("enabled".to_owned(), toml::Value::Boolean(true));
    for (key, value) in extra {
        options.insert((*key).to_owned(), value.clone());
    }
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options,
    }
}

fn language_of(path: &Path) -> Option<Language> {
    match path.extension()?.to_str()? {
        "json" => Some(Language::Json),
        "yaml" => Some(Language::Yaml),
        "toml" => Some(Language::Toml),
        "js" => Some(Language::JavaScript),
        _ => None,
    }
}

/// Every file under `root` of a language skif serves, the way the host hands them over.
fn load(root: &Path) -> Vec<ProjectFile> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if let Some(language) = language_of(&path) {
                files.push(ProjectFile {
                    content: fs::read_to_string(&path).expect("read").into(),
                    path,
                    language,
                });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

fn run(root: &Path, files: &[ProjectFile], config: &EngineConfig) -> Analysis {
    let environment = ProjectEnvironment::default();
    let request = ProjectRequest {
        root,
        files,
        config,
        environment: &environment,
    };
    analyze(&request).expect("skif runs")
}

/// `(path relative to root, rule, severity, line)` of every finding, sorted.
fn summary(root: &Path, analysis: &Analysis) -> Vec<(String, String, String, Option<u32>)> {
    let mut found: Vec<_> = analysis
        .findings
        .iter()
        .flat_map(|(path, diagnostics)| {
            let relative = path.strip_prefix(root).expect("inside the root").display().to_string();
            diagnostics.iter().map(move |d| {
                (
                    relative.clone(),
                    d.code.clone().expect("a rule id"),
                    format!("{:?}", d.severity),
                    d.span.map(|span| span.start_line),
                )
            })
        })
        .collect();
    found.sort();
    found
}

fn expected() -> Vec<(String, String, String, Option<u32>)> {
    let row =
        |file: &str, rule: &str, severity: &str, line| (file.to_owned(), rule.to_owned(), severity.to_owned(), line);
    vec![
        row("package.json", "root-package-private-field", "Error", Some(1)),
        row("packages/a/package.json", "empty-dependencies", "Error", Some(7)),
        row("packages/b/package.json", "catalog-mismatch", "Warning", Some(5)),
    ]
}

#[test]
fn a_known_monorepo_has_exactly_these_findings() {
    let root = fixture();
    let analysis = run(&root, &load(&root), &cfg(&[]));
    assert_eq!(summary(&root, &analysis), expected());
}

#[test]
fn every_file_skif_read_was_an_input_of_the_request() {
    let root = fixture();
    let files = load(&root);
    let analysis = run(&root, &files, &cfg(&[]));
    let handed: std::collections::BTreeSet<PathBuf> = files.iter().map(|file| file.path.clone()).collect();
    let stray: Vec<_> = analysis.log.read.difference(&handed).collect();
    assert!(
        stray.is_empty(),
        "skif read files the cache key does not cover: {stray:?}"
    );
    for member in [
        "package.json",
        "packages/a/package.json",
        "packages/b/package.json",
        "pnpm-workspace.yaml",
    ] {
        assert!(
            analysis.log.read.contains(&root.join(member)),
            "skif never read {member}: {:?}",
            analysis.log.read
        );
    }
}

#[test]
fn a_file_missing_from_the_request_is_not_found_on_disk() {
    let root = fixture();
    let without_b: Vec<_> = load(&root)
        .into_iter()
        .filter(|file| !file.path.starts_with(root.join("packages/b")))
        .collect();
    let analysis = run(&root, &without_b, &cfg(&[]));
    let found = summary(&root, &analysis);
    assert!(
        found.iter().all(|(file, ..)| !file.starts_with("packages/b")),
        "b exists on disk but not in the request, so skif must not see it: {found:?}"
    );
    assert!(
        analysis
            .log
            .read
            .iter()
            .all(|path| !path.starts_with(root.join("packages/b"))),
        "{:?}",
        analysis.log.read
    );
}

#[test]
fn a_project_without_a_workspace_reports_nothing() {
    let root = fixture();
    let sources: Vec<_> = load(&root)
        .into_iter()
        .filter(|file| file.language == Language::JavaScript)
        .collect();
    assert!(!sources.is_empty());
    assert!(run(&root, &sources, &cfg(&[])).findings.is_empty());
}

#[test]
fn it_does_nothing_unless_enabled() {
    let root = fixture();
    let mut disabled = cfg(&[]);
    disabled.options.remove("enabled");
    assert!(run(&root, &load(&root), &disabled).findings.is_empty());
    disabled
        .options
        .insert("enabled".to_owned(), toml::Value::Boolean(false));
    assert!(run(&root, &load(&root), &disabled).findings.is_empty());
}

#[test]
fn ignore_and_select_narrow_the_rules() {
    let root = fixture();
    let files = load(&root);
    let rules = |config: &EngineConfig| -> Vec<String> {
        summary(&root, &run(&root, &files, config))
            .into_iter()
            .map(|row| row.1)
            .collect()
    };
    let ignore = toml::Value::Array(vec!["empty-dependencies".into()]);
    assert_eq!(
        rules(&cfg(&[("ignore", ignore)])),
        ["root-package-private-field", "catalog-mismatch"]
    );
    let select = toml::Value::Array(vec!["catalog-mismatch".into()]);
    assert_eq!(rules(&cfg(&[("select", select)])), ["catalog-mismatch"]);
}

#[test]
fn a_bad_option_is_an_error_naming_it() {
    let root = fixture();
    let files = load(&root);
    let environment = ProjectEnvironment::default();
    for (key, value, needle) in [
        ("threads", toml::Value::Integer(0), "threads"),
        ("dir", toml::Value::Integer(3), "dir"),
        ("ignore", toml::Value::String("x".into()), "ignore"),
        (
            "select",
            toml::Value::Array(vec!["no-such-rule".into()]),
            "no-such-rule",
        ),
    ] {
        let config = cfg(&[(key, value)]);
        let request = ProjectRequest {
            root: &root,
            files: &files,
            config: &config,
            environment: &environment,
        };
        let error = analyze(&request).expect_err(key);
        assert!(format!("{error:#}").contains(needle), "{key}: {error:#}");
    }
}

#[test]
fn only_safe_single_file_fixes_carry_edits() {
    let root = fixture();
    let analysis = run(&root, &load(&root), &cfg(&[]));
    let edits: Vec<(String, usize)> = analysis
        .findings
        .iter()
        .flat_map(|(_, diagnostics)| diagnostics.iter())
        .map(|d| (d.code.clone().unwrap(), d.fix.len()))
        .collect();
    let has = |rule: &str| edits.iter().find(|(code, _)| code == rule).map(|(_, count)| *count);
    assert!(
        has("root-package-private-field").is_some_and(|count| count > 0),
        "{edits:?}"
    );
    assert!(has("empty-dependencies").is_some_and(|count| count > 0), "{edits:?}");
    assert_eq!(
        has("catalog-mismatch"),
        Some(0),
        "an unsafe fix is never offered: {edits:?}"
    );
}

/// Drives `poly_core::lint_run` over a copy of the fixture with the cache **on**.
struct Project {
    dir: tempfile::TempDir,
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("mkdir");
    for entry in fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy");
        }
    }
}

impl Project {
    fn new(config: &str) -> Self {
        static CACHE_HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        CACHE_HOME.get_or_init(|| {
            let dir = tempfile::tempdir().expect("cache dir");
            // SAFETY: set once, from the `OnceLock` initialiser, before any run in this binary reads it.
            unsafe {
                std::env::set_var("POLY_CACHE_HOME", dir.path());
            }
            dir
        });
        let dir = tempfile::tempdir().expect("project");
        copy_tree(&fixture(), dir.path());
        fs::write(
            dir.path().join("poly.toml"),
            format!("[workspace]\nroot = true\n\n{config}"),
        )
        .expect("config");
        Self { dir }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    fn lint(&self, fix: bool) -> poly_core::LintRun {
        common::register();
        let options = poly_core::RunOptions {
            jobs: Some(1),
            ..poly_core::RunOptions::default()
        };
        let run = poly_core::lint_run(
            &[self.dir.path().to_path_buf()],
            &poly_core::Config::default(),
            &options,
            fix,
            false,
        )
        .expect("lint run");
        assert!(run.errors.is_empty(), "{:?}", run.errors);
        run
    }

    /// `(file, rule)` of every skif finding left in `run`.
    fn skif(&self, run: &poly_core::LintRun) -> Vec<(String, String)> {
        let mut found: Vec<_> = run
            .results
            .iter()
            .flat_map(|result| {
                let name = result
                    .path
                    .strip_prefix(self.dir.path())
                    .expect("inside")
                    .display()
                    .to_string();
                result
                    .diagnostics
                    .iter()
                    .filter(|d| d.engine == "skif")
                    .map(move |d| (name.clone(), d.code.clone().unwrap_or_default()))
            })
            .collect();
        found.sort();
        found
    }
}

const ENABLED: &str = "[engines.skif]\nenabled = true\n";

fn skif_rules(found: &[(String, String)]) -> Vec<&str> {
    found.iter().map(|(_, rule)| rule.as_str()).collect()
}

#[test]
fn the_pipeline_reports_the_findings_only_when_enabled() {
    let off = Project::new("");
    assert_eq!(off.skif(&off.lint(false)), []);
    let on = Project::new(ENABLED);
    let found = on.skif(&on.lint(false));
    assert_eq!(
        found,
        [
            ("package.json".to_owned(), "root-package-private-field".to_owned()),
            ("packages/a/package.json".to_owned(), "empty-dependencies".to_owned()),
            ("packages/b/package.json".to_owned(), "catalog-mismatch".to_owned()),
        ]
    );
}

#[test]
fn fix_applies_the_safe_fixes_and_leaves_the_unsafe_one() {
    let project = Project::new(ENABLED);
    let b_before = fs::read_to_string(project.path("packages/b/package.json")).unwrap();
    let run = project.lint(true);
    let fixed: usize = run.results.iter().map(|result| result.fixed).sum();
    assert_eq!(fixed, 2, "root-package-private-field and empty-dependencies");
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(project.path("package.json")).unwrap()).unwrap();
    assert_eq!(root["private"], true);
    let a = fs::read_to_string(project.path("packages/a/package.json")).unwrap();
    assert!(!a.contains("devDependencies"), "{a}");
    assert_eq!(
        fs::read_to_string(project.path("packages/b/package.json")).unwrap(),
        b_before,
        "catalog-mismatch needs a decision, so --fix leaves it"
    );
    assert_eq!(
        skif_rules(&project.skif(&run)),
        ["catalog-mismatch"],
        "what is left in the report"
    );
    assert_eq!(
        skif_rules(&project.skif(&project.lint(false))),
        ["catalog-mismatch"],
        "and on a fresh run"
    );
}

#[test]
fn editing_a_member_manifest_invalidates_the_cached_result() {
    let project = Project::new(ENABLED);
    let first = project.skif(&project.lint(false));
    assert_eq!(skif_rules(&first).len(), 3);
    assert_eq!(
        project.skif(&project.lint(false)),
        first,
        "an unchanged tree is served from the cache"
    );

    fs::write(
        project.path("packages/a/package.json"),
        "{\n  \"name\": \"@e2e/a\",\n  \"version\": \"0.0.0\",\n  \"dependencies\": {\n    \"is-number\": \"catalog:\"\n  }\n}\n",
    )
    .unwrap();
    assert_eq!(
        skif_rules(&project.skif(&project.lint(false))),
        ["root-package-private-field", "catalog-mismatch"],
        "a member package.json changed, so the verdict must be recomputed"
    );
}

#[test]
fn editing_the_catalog_invalidates_the_cached_result() {
    let project = Project::new(ENABLED);
    assert_eq!(skif_rules(&project.skif(&project.lint(false))).len(), 3);
    let workspace = fs::read_to_string(project.path("pnpm-workspace.yaml")).unwrap();
    fs::write(
        project.path("pnpm-workspace.yaml"),
        workspace.replace("is-number: 7.0.0", "is-odd: 3.0.1"),
    )
    .unwrap();
    let after = project.skif(&project.lint(false));
    assert_eq!(
        skif_rules(&after),
        [
            "root-package-private-field",
            "empty-dependencies",
            "unresolved-catalog-references"
        ],
        "the catalog lost the entry a depends on"
    );
}

#[test]
fn skif_toml_is_honored() {
    let project = Project::new(ENABLED);
    fs::write(project.path("skif.toml"), "[rules]\nempty-dependencies = \"off\"\n").unwrap();
    let found = project.skif(&project.lint(false));
    assert!(!skif_rules(&found).contains(&"empty-dependencies"), "{found:?}");
    assert!(skif_rules(&found).contains(&"catalog-mismatch"), "{found:?}");
}
