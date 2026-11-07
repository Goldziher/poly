//! The web engines' declared hooks (`Engine::overridden_hooks`, recorded in the component index) must
//! cover every per-file hook method they override. The root workspace's `tests/component_hooks.rs`
//! runs the same check over the engines `poly-core` registers.
//!
//! The host answers an undeclared hook with the trait default without loading the component, so an
//! engine that overrides a hook it did not declare would silently get the default in a component
//! build. Each undeclared hook is exercised against a default-only twin of the engine over sample
//! files and configs and must give the same answer.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::desc::hooks;
use poly_engine_api::engine::{Capabilities, Engine, SourceFile};
use poly_engine_api::host_hooks::HostHook;
use poly_engine_api::language::Language;
use poly_engines_web::engines_for;

/// The engine's statics and host hook with every per-file hook left at the trait default.
struct Defaults<'a>(&'a dyn Engine);

impl Engine for Defaults<'_> {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn languages(&self) -> &'static [Language] {
        self.0.languages()
    }
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }
    fn version(&self) -> &str {
        self.0.version()
    }
    fn host_hook(&self) -> HostHook {
        self.0.host_hook()
    }
}

const PATHS: &[&str] = &[
    "a.py",
    "a.ts",
    "a.js",
    "a.json",
    "a.css",
    "a.scss",
    "README.md",
    "docs/adr/0001-choice.md",
    "adrs/0002-other.md",
    "a.yaml",
    "chart/templates/a.yaml",
    "a.toml",
    "a.html",
    "a.vue",
    "a.jinja",
    "a.graphql",
    "Dockerfile",
    ".env",
    "main.tf",
    "a.rb",
    "a.nix",
    "a.sql",
    "a.php",
    "a.rs",
    "a.go",
    "a.sh",
];

const CONTENTS: &[&str] = &[
    "",
    "x: 1\n",
    "{{ .Values.a }}\n{{- if .x }}\n",
    "# generated\n<% foo %>\n",
    "# ADR\n\nStatus: accepted\n",
    "FROM scratch\n",
];

const OPTIONS: &[&str] = &["", "batch = true", "enabled = false", "select = [\"ALL\"]"];

fn config(options: &str) -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::from_str(options).expect("valid options"),
    }
}

fn sources() -> Vec<SourceFile> {
    let mut files = Vec::new();
    for path in PATHS {
        let Some(language) = Language::from_path(Path::new(path)) else {
            continue;
        };
        for content in CONTENTS {
            files.push(SourceFile {
                path: (*path).into(),
                language: language.clone(),
                content: Arc::from(*content),
            });
        }
    }
    files
}

/// Hooks `engine` overrides without declaring them, with the sample count checked.
fn undeclared_overrides(engine: &dyn Engine) -> (Vec<String>, usize) {
    let declared = engine.overridden_hooks();
    let twin = Defaults(engine);
    let files = sources();
    let configs: Vec<EngineConfig> = OPTIONS.iter().map(|o| config(o)).collect();
    let mut found = Vec::new();
    let mut checked = 0;
    let mut check = |bit: u32, name: &str, same: &dyn Fn() -> bool| {
        if declared & bit != 0 {
            return;
        }
        checked += 1;
        if !same() {
            found.push(format!("{} overrides undeclared hook `{name}`", engine.name()));
        }
    };
    check(hooks::RUN_CONTEXT, "run_context", &|| {
        configs.iter().all(|c| engine.run_context(c) == twin.run_context(c))
    });
    check(hooks::FILE_CONTEXT, "file_context", &|| {
        files.iter().all(|f| {
            configs
                .iter()
                .all(|c| engine.file_context(&f.path, c) == twin.file_context(&f.path, c))
        })
    });
    check(hooks::PROVIDES_LANGUAGE, "provides_language_lint/format", &|| {
        files.iter().all(|f| {
            configs.iter().all(|c| {
                engine.provides_language_lint(&f.language, c) == twin.provides_language_lint(&f.language, c)
                    && engine.provides_language_format(&f.language, c) == twin.provides_language_format(&f.language, c)
            })
        })
    });
    check(hooks::SKIP_REASON, "skip_reason", &|| {
        files.iter().all(|f| engine.skip_reason(f) == twin.skip_reason(f))
    });
    check(hooks::LINT_INPUT_SALT, "lint_input_salt", &|| {
        files.iter().all(|f| {
            configs
                .iter()
                .all(|c| engine.lint_input_salt(&f.path, c) == twin.lint_input_salt(&f.path, c))
        })
    });
    check(hooks::FORMAT_INPUT_SALT, "format_input_salt", &|| {
        files.iter().all(|f| {
            configs
                .iter()
                .all(|c| engine.format_input_salt(&f.path, c) == twin.format_input_salt(&f.path, c))
        })
    });
    check(hooks::LINT_CACHE_SALT, "lint_cache_salt", &|| {
        files
            .iter()
            .all(|f| engine.lint_cache_salt(f) == twin.lint_cache_salt(f))
    });
    check(hooks::BATCH_SUPPORT, "batch_support", &|| {
        configs.iter().all(|c| engine.batch_support(c) == twin.batch_support(c))
    });
    (found, checked)
}

/// One instance of every web engine, found through the languages the sample files cover.
fn web_engines() -> Vec<Box<dyn Engine>> {
    let mut seen = BTreeSet::new();
    let mut engines = Vec::new();
    for file in sources() {
        for engine in engines_for(&file.language) {
            if seen.insert(engine.name()) {
                engines.push(engine);
            }
        }
    }
    engines
}

#[test]
fn web_engines_declare_every_hook_they_override() {
    let engines = web_engines();
    assert_eq!(
        engines.len(),
        8,
        "every web engine must be reached by a sample file, got {:?}",
        engines.iter().map(|e| e.name()).collect::<Vec<_>>()
    );
    let mut narrowed = 0;
    for engine in engines {
        let (found, checked) = undeclared_overrides(engine.as_ref());
        assert!(found.is_empty(), "{found:?}");
        if engine.overridden_hooks() != poly_engine_api::desc::ALL_HOOKS {
            assert!(
                checked > 0,
                "{} declares fewer hooks yet nothing was checked",
                engine.name()
            );
            narrowed += 1;
        }
    }
    assert!(
        narrowed > 0,
        "no web engine narrows its hooks; the check examined nothing"
    );
}
