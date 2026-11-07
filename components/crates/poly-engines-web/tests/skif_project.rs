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
use poly_engines_web::skif::{Analysis, DEFAULTED_OFF_CODE, analyze};

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

/// The language the host gives `path`, for what the engine serves or declared by name; `None` for the rest.
fn language_of(path: &Path) -> Option<Language> {
    let name = path.file_name()?.to_str()?;
    if matches!(name, ".gitignore" | ".ignore" | "yarn.lock" | "bun.lock") {
        return Some(Language::Other(name.to_owned()));
    }
    match path.extension()?.to_str()? {
        "json" => Some(Language::Json),
        "yaml" => Some(Language::Yaml),
        "toml" => Some(Language::Toml),
        "js" => Some(Language::JavaScript),
        "ts" => Some(Language::TypeScript),
        "vue" => Some(Language::Vue),
        "svelte" => Some(Language::Svelte),
        "astro" => Some(Language::Astro),
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
        threads: None,
    };
    analyze(&request).expect("skif runs")
}

/// `path` with `/` separators on every OS, so expectations are written once.
fn slashed(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// `(path relative to root, rule, severity, line)` of every finding, sorted.
fn summary(root: &Path, analysis: &Analysis) -> Vec<(String, String, String, Option<u32>)> {
    let mut found: Vec<_> = analysis
        .findings
        .iter()
        .flat_map(|(path, diagnostics)| {
            let relative = slashed(path.strip_prefix(root).expect("inside the root"));
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

/// The paths skif looked for under `root` and did not find, relative to it, and those above it.
fn missed(root: &Path, analysis: &Analysis) -> (Vec<String>, Vec<PathBuf>) {
    let (inside, outside): (Vec<_>, Vec<_>) = analysis.log.missed.iter().partition(|path| path.starts_with(root));
    let inside = inside
        .into_iter()
        .map(|path| slashed(path.strip_prefix(root).unwrap()))
        .collect();
    (inside, outside.into_iter().cloned().collect())
}

fn directories_of(files: &[ProjectFile]) -> std::collections::BTreeSet<PathBuf> {
    files
        .iter()
        .flat_map(|file| file.path.ancestors().skip(1).map(Path::to_path_buf))
        .collect()
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
    let implied = directories_of(&files);
    let unknown_dirs: Vec<_> = analysis.log.listed.difference(&implied).collect();
    assert!(
        unknown_dirs.is_empty(),
        "skif listed directories the request does not imply: {unknown_dirs:?}"
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

/// What skif looked for in the workspace fixture and the request does not hold: a clean list of the inputs a
/// project could add that would change the verdict, none of them one the project already has.
const ABSENT_BY_DESIGN: &[&str] = &[
    "",
    ".git",
    ".skif.toml",
    "MODULE.bazel",
    "WORKSPACE",
    "WORKSPACE.bazel",
    "bun.lock",
    "node_modules/vue/package.json",
    "pnpm-workspace.yaml",
    "rush.json",
    "skif.toml",
    "src/App.vue/tsconfig.json",
    "src/generated",
    "src/generated/api",
    "src/generated/api.ts",
    "src/main.ts/tsconfig.json",
    "yarn.lock",
];

/// Names skif probes in the ancestors of the project root for a configuration to inherit. They cannot be keyed:
/// nothing above the root is handed over, so a `skif.toml` there is not read.
const ANCESTOR_PROBES: &[&str] = &[
    ".git",
    ".skif.toml",
    "package.json",
    "pnpm-workspace.yaml",
    "skif.toml",
    "tsconfig.json",
];

fn gitignore_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skif/ws-gitignore")
}

#[test]
fn what_skif_looks_for_and_does_not_get_is_exactly_what_is_intentionally_absent() {
    let root = gitignore_fixture();
    let files = load(&root);
    let analysis = run(&root, &files, &cfg(&[]));
    let (inside, outside) = missed(&root, &analysis);
    assert_eq!(
        inside, ABSENT_BY_DESIGN,
        "a needed input was not handed over, or skif reads something new"
    );
    let ancestors: Vec<_> = root.ancestors().skip(1).collect();
    for path in &outside {
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        assert!(
            ancestors.contains(&path.as_path())
                || (path.parent().is_some_and(|dir| ancestors.contains(&dir)) && ANCESTOR_PROBES.contains(&name)),
            "a probe outside the project that is not an ancestor lookup: {}",
            path.display()
        );
    }
    for input in [".gitignore", "tsconfig.json", "package.json"] {
        assert!(analysis.log.read.contains(&root.join(input)), "{input} was not read");
        assert!(
            !inside.iter().any(|path| path == input),
            "{input} was handed over and still reported missing"
        );
    }
}

#[test]
fn an_input_that_is_not_handed_over_shows_up_as_missed_and_changes_the_result() {
    let root = gitignore_fixture();
    let without_gitignore: Vec<_> = load(&root)
        .into_iter()
        .filter(|file| file.path.file_name().is_none_or(|name| name != ".gitignore"))
        .collect();
    let analysis = run(&root, &without_gitignore, &cfg(&[]));
    assert!(
        !analysis.log.read.contains(&root.join(".gitignore")),
        "skif cannot have read a .gitignore it was not handed: {:?}",
        analysis.log.read
    );
    assert_eq!(
        summary(&root, &analysis),
        [(
            "src/main.ts".to_owned(),
            "unresolved-imports".to_owned(),
            "Error".to_owned(),
            Some(1)
        )],
        "the generated import is unresolved once skif cannot see the .gitignore that excuses it"
    );
}

#[test]
fn gitignored_generated_imports_and_component_imports_are_not_findings() {
    let root = gitignore_fixture();
    let analysis = run(&root, &load(&root), &cfg(&[]));
    assert_eq!(summary(&root, &analysis), []);
}

#[test]
fn component_files_are_part_of_the_graph() {
    let root = gitignore_fixture();
    let without_components: Vec<_> = load(&root)
        .into_iter()
        .filter(|file| !matches!(file.language, Language::Vue | Language::Svelte | Language::Astro))
        .collect();
    let found = summary(&root, &run(&root, &without_components, &cfg(&[])));
    let row = |file: &str, rule: &str, line| (file.to_owned(), rule.to_owned(), "Error".to_owned(), line);
    assert_eq!(
        found,
        [
            row("src/main.ts", "unresolved-imports", Some(2)),
            row("src/main.ts", "unresolved-imports", Some(3)),
            row("src/main.ts", "unresolved-imports", Some(4)),
            (
                "src/util.ts".to_owned(),
                "unused-files".to_owned(),
                "Warning".to_owned(),
                None
            ),
        ],
        "the Vue, Svelte and Astro imports only resolve when those files are served"
    );
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
            threads: None,
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

/// A workspace written inline, under a root that does not exist on disk: what the host would hand over.
fn inline(files: &[(&str, &str)]) -> (PathBuf, Vec<ProjectFile>) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("virtual-workspace");
    let files = files
        .iter()
        .map(|(path, content)| ProjectFile {
            path: root.join(path),
            language: language_of(Path::new(path)).expect("a language skif serves"),
            content: (*content).into(),
        })
        .collect();
    (root, files)
}

const ROOT: &str = r#"{"name":"r","version":"1.0.0","private":true,"packageManager":"pnpm@9.0.0"}"#;
const PACKAGES: &str = "packages:\n  - 'packages/*'\n";

/// `@acme/toolkit` is only run as `kit`, and `@acme/peer` is only the peer `@acme/host` asks for. Neither can be
/// known without the installed packages or a lock file that names them.
fn unknowable_members() -> Vec<(&'static str, &'static str)> {
    vec![
        ("package.json", ROOT),
        ("pnpm-workspace.yaml", PACKAGES),
        (
            "packages/tool/package.json",
            r#"{"name":"tool","version":"1.0.0","devDependencies":{"@acme/toolkit":"1.0.0"},"scripts":{"build":"kit build"}}"#,
        ),
        (
            "packages/host/package.json",
            r#"{"name":"host","version":"1.0.0","main":"src/a.ts","dependencies":{"@acme/host":"1.0.0","@acme/peer":"1.0.0"}}"#,
        ),
        (
            "packages/host/src/a.ts",
            "import host from \"@acme/host\";\nconsole.log(host);\n",
        ),
    ]
}

fn rules_found(root: &Path, files: &[ProjectFile], config: &EngineConfig) -> Vec<(String, String)> {
    summary(root, &run(root, files, config))
        .into_iter()
        .map(|(file, rule, ..)| (file, rule))
        .collect()
}

fn pair(file: &str, rule: &str) -> (String, String) {
    (file.to_owned(), rule.to_owned())
}

fn select(rule: &str) -> (&'static str, toml::Value) {
    ("select", toml::Value::Array(vec![rule.into()]))
}

#[test]
fn rules_that_need_installed_packages_are_off_unless_asked_for() {
    let (root, files) = inline(&unknowable_members());
    assert_eq!(
        rules_found(&root, &files, &cfg(&[])),
        [],
        "without node_modules these report a dependency as unused because skif cannot see what it provides"
    );
    for (rule, expected) in [
        (
            "unlisted-binaries",
            pair("packages/tool/package.json", "unlisted-binaries"),
        ),
        (
            "unused-dev-dependencies",
            pair("packages/tool/package.json", "unused-dev-dependencies"),
        ),
        (
            "unused-dependencies",
            pair("packages/host/package.json", "unused-dependencies"),
        ),
    ] {
        assert_eq!(
            rules_found(&root, &files, &cfg(&[select(rule)])),
            [expected],
            "select = [{rule}]"
        );
    }
}

#[test]
fn a_rule_table_or_the_projects_skif_toml_asks_for_a_rule_too() {
    let (root, files) = inline(&unknowable_members());
    let rules: toml::Table = toml::from_str("[unlisted-binaries]\nlevel = \"warning\"\n").unwrap();
    assert_eq!(
        rules_found(&root, &files, &cfg(&[("rules", toml::Value::Table(rules))])),
        [pair("packages/tool/package.json", "unlisted-binaries")]
    );
    let mut with_toml = unknowable_members();
    with_toml.push(("skif.toml", "[rules]\nunused-dependencies = \"error\"\n"));
    let (root, files) = inline(&with_toml);
    assert_eq!(
        rules_found(&root, &files, &cfg(&[])),
        [pair("packages/host/package.json", "unused-dependencies")]
    );
}

#[test]
fn a_lock_file_that_names_the_peers_removes_the_false_unused_dependency() {
    let lock = "lockfileVersion: '9.0'\n\npackages:\n\n  '@acme/host@1.0.0':\n    resolution: {integrity: sha512-x}\n    \
                peerDependencies:\n      '@acme/peer': ^1.0.0\n";
    let mut members = unknowable_members();
    members.push(("pnpm-lock.yaml", lock));
    let (root, files) = inline(&members);
    assert_eq!(rules_found(&root, &files, &cfg(&[select("unused-dependencies")])), []);
}

/// `unknowable_members` plus a dependency nothing uses, and the lock file `lock` (named `name`) that records what
/// `@acme/toolkit` and `@acme/host` provide.
fn with_lock(name: &str, lock: &str) -> (PathBuf, Vec<ProjectFile>) {
    let mut members = unknowable_members();
    members.retain(|(path, _)| *path != "packages/host/package.json");
    members.push((
        "packages/host/package.json",
        r#"{"name":"host","version":"1.0.0","main":"src/a.ts","dependencies":{"@acme/host":"1.0.0","@acme/peer":"1.0.0","left-pad":"1.0.0"}}"#,
    ));
    members.push((name, lock));
    inline(&members)
}

const NPM_LOCK: &str = r#"{"name":"r","lockfileVersion":3,"packages":{
  "node_modules/@acme/toolkit":{"version":"1.0.0","bin":{"kit":"cli.js"}},
  "node_modules/@acme/host":{"version":"1.0.0","peerDependencies":{"@acme/peer":"1.0.0"}},
  "node_modules/@acme/peer":{"version":"1.0.0"},
  "node_modules/left-pad":{"version":"1.0.0"}}}"#;

const BERRY_LOCK: &str = "__metadata:\n  version: 8\n\n\"@acme/toolkit@npm:1.0.0\":\n  version: 1.0.0\n  bin:\n    kit: cli.js\n  \
     languageName: node\n\n\"@acme/host@npm:1.0.0\":\n  version: 1.0.0\n  peerDependencies:\n    \"@acme/peer\": ^1.0.0\n  \
     languageName: node\n\n\"@acme/peer@npm:1.0.0\":\n  version: 1.0.0\n  languageName: node\n\n\"left-pad@npm:1.0.0\":\n  \
     version: 1.0.0\n  languageName: node\n";

const CLASSIC_YARN_LOCK: &str =
    "# yarn lockfile v1\n\n\"@acme/toolkit@1.0.0\":\n  version \"1.0.0\"\n\nleft-pad@1.0.0:\n  version \"1.0.0\"\n";

#[test]
fn a_lock_file_that_records_binaries_keeps_the_unused_dependency_rules_on_by_default() {
    for (name, lock) in [("package-lock.json", NPM_LOCK), ("yarn.lock", BERRY_LOCK)] {
        let (root, files) = with_lock(name, lock);
        assert_eq!(
            rules_found(&root, &files, &cfg(&[])),
            [pair("packages/host/package.json", "unused-dependencies")],
            "{name}: the lock names kit and the peer, so only left-pad is unused"
        );
    }
}

#[test]
fn without_a_lock_file_that_records_binaries_the_unused_dependency_rules_stay_off() {
    for (name, lock) in [
        ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
        ("yarn.lock", CLASSIC_YARN_LOCK),
    ] {
        let (root, files) = with_lock(name, lock);
        assert_eq!(rules_found(&root, &files, &cfg(&[])), [], "{name}");
        assert_eq!(
            rules_found(&root, &files, &cfg(&[select("unused-dependencies")])),
            [
                pair("packages/host/package.json", "unused-dependencies"),
                pair("packages/host/package.json", "unused-dependencies")
            ],
            "{name}: asking for the rule turns it back on, and the peer is then reported as unused too"
        );
    }
}

#[test]
fn the_rules_left_off_are_named_in_one_info_note_on_the_root_manifest() {
    let (root, files) = inline(&unknowable_members());
    let analysis = run(&root, &files, &cfg(&[]));
    let (path, note) = analysis.note.expect("rules were left off");
    assert_eq!(path, root.join("package.json"));
    assert_eq!(note.code.as_deref(), Some(DEFAULTED_OFF_CODE));
    assert_eq!(format!("{:?}", note.severity), "Info");
    for rule in ["unlisted-binaries", "unused-dependencies", "unused-dev-dependencies"] {
        assert!(note.title.contains(rule), "{}", note.title);
    }
    assert!(note.description.as_deref().is_some_and(|how| how.contains("select")));

    let (root, files) = with_lock("package-lock.json", NPM_LOCK);
    let note = run(&root, &files, &cfg(&[]))
        .note
        .expect("unlisted-binaries is still off")
        .1;
    assert!(note.title.contains("unlisted-binaries") && !note.title.contains("unused-dependencies"));

    let everything = toml::Value::Array(
        ["unlisted-binaries", "unused-dependencies", "unused-dev-dependencies"]
            .map(toml::Value::from)
            .to_vec(),
    );
    let (root, files) = inline(&unknowable_members());
    assert!(run(&root, &files, &cfg(&[("select", everything)])).note.is_none());
}

#[test]
fn a_binary_skif_knows_by_name_is_not_reported_when_the_rule_is_asked_for() {
    let (root, files) = inline(&[
        ("package.json", ROOT),
        ("pnpm-workspace.yaml", PACKAGES),
        (
            "packages/app/package.json",
            r#"{"name":"app","version":"1.0.0","devDependencies":{"@angular/cli":"17.0.0"},"scripts":{"start":"ng serve"}}"#,
        ),
    ]);
    assert_eq!(rules_found(&root, &files, &cfg(&[select("unlisted-binaries")])), []);
}

#[test]
fn a_tsconfig_extending_into_a_package_does_not_make_types_unused() {
    let (root, files) = inline(&[
        ("package.json", ROOT),
        ("pnpm-workspace.yaml", PACKAGES),
        (
            "packages/app/package.json",
            r#"{"name":"app","version":"1.0.0","main":"src/a.ts","devDependencies":{"@tsconfig/node20":"20.0.0","@types/jest":"29.0.0","@types/node":"20.0.0","typescript":"5.0.0"},"scripts":{"build":"tsc"}}"#,
        ),
        (
            "packages/app/tsconfig.json",
            r#"{"extends":"@tsconfig/node20/tsconfig.json"}"#,
        ),
        ("packages/app/src/a.ts", "export const a: number = 1;\n"),
    ]);
    assert_eq!(
        rules_found(&root, &files, &cfg(&[select("unused-dev-dependencies")])),
        [],
        "the base config is not installed, so skif assumes it restricts nothing: a missed finding, never a false one"
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
        Self::from_fixture(&fixture(), config)
    }

    fn from_fixture(source: &Path, config: &str) -> Self {
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
        copy_tree(source, dir.path());
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
                let name = slashed(result.path.strip_prefix(self.dir.path()).expect("inside"));
                result
                    .diagnostics
                    .iter()
                    .filter(|d| d.engine == "skif" && d.code.as_deref() != Some(DEFAULTED_OFF_CODE))
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

#[test]
fn the_host_walk_hands_over_components_and_ignore_files_and_ignores_discovery_excludes() {
    let config = format!("{ENABLED}\n[discovery]\nexclude = [\"src/util.ts\"]\n");
    let project = Project::from_fixture(&gitignore_fixture(), &config);
    let run = project.lint(false);
    assert_eq!(
        project.skif(&run),
        [],
        "src/util.ts is excluded from linting but still a node of the import graph: App.vue imports it"
    );
    assert!(run.errors.is_empty());
}

#[test]
fn the_pipeline_reports_the_defaulting_note_once_per_run() {
    let project = Project::new(ENABLED);
    let run = project.lint(false);
    let notes: Vec<_> = run
        .results
        .iter()
        .flat_map(|result| &result.diagnostics)
        .filter(|d| d.code.as_deref() == Some(DEFAULTED_OFF_CODE))
        .collect();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(format!("{:?}", notes[0].severity), "Info");
    let off = Project::new("");
    assert!(
        off.lint(false)
            .results
            .iter()
            .flat_map(|result| &result.diagnostics)
            .all(|d| d.engine != "skif"),
        "a project that did not enable skif hears nothing from it"
    );
}

#[test]
fn a_lock_file_skif_gets_no_packages_from_does_not_keep_the_unused_dependency_rules_on() {
    let v1 = r#"{"name":"r","lockfileVersion":1,"dependencies":{"left-pad":{"version":"1.0.0"}}}"#;
    for (name, lock) in [
        ("package-lock.json", v1),
        ("package-lock.json", "{ not json"),
        ("bun.lock", "{ not json"),
    ] {
        let (root, files) = with_lock(name, lock);
        let analysis = run(&root, &files, &cfg(&[]));
        assert_eq!(summary(&root, &analysis).len(), 0, "{name} {lock}: the rules stay off");
        let note = analysis.note.expect("rules were left off").1;
        assert!(note.title.contains("unused-dependencies"), "{name}: {}", note.title);
    }
    let (root, files) = with_lock("package-lock.json", NPM_LOCK);
    assert_eq!(
        rules_found(&root, &files, &cfg(&[])),
        [pair("packages/host/package.json", "unused-dependencies")]
    );
}

#[test]
fn a_rule_the_project_ignored_is_not_named_in_the_defaulting_note() {
    let (root, files) = inline(&unknowable_members());
    let ignore = ("ignore", toml::Value::Array(vec!["unused-dependencies".into()]));
    let note = run(&root, &files, &cfg(&[ignore]))
        .note
        .expect("other rules were left off")
        .1;
    assert!(!note.title.contains("`unused-dependencies`"), "{}", note.title);
    assert!(note.title.contains("`unlisted-binaries`"), "{}", note.title);

    let all = ["unlisted-binaries", "unused-dependencies", "unused-dev-dependencies"].map(toml::Value::from);
    let ignore = ("ignore", toml::Value::Array(all.to_vec()));
    assert!(run(&root, &files, &cfg(&[ignore])).note.is_none());
}
