//! Custom-rule lint + autofix engine built on `ast-grep-core`.
//!
//! [`AstGrepEngine`] is a cross-cutting backend (registered after all
//! language-specific engines, similar to [`crate::engines::typos::TyposEngine`])
//! that runs two rule sources against every file whose language has matching
//! rules:
//!
//! 1. poly's embedded built-in rule pack ([`pack`]) — on by default, no setup
//!    required.
//! 2. user-authored YAML rules loaded from the configured `[rules] dirs`
//!    directories.
//!
//! The pack is layered *beneath* user rules: a user rule with the same `id`
//! as a pack rule replaces it. See [`pack`] for the pack's own docs (parsing,
//! layering, and the path-exclusion problem for a handful of noisy rules).
//!
//! ## Rule format
//!
//! Rules are standard `ast-grep` YAML files.  Each rule specifies a
//! `language:` field that must match a grammar name known to
//! `tree-sitter-language-pack` (e.g. `python`, `go`, `javascript`).  A file
//! may contain multiple rules; multiple files are merged into one rule set.
//!
//! ```yaml
//! id: no-print
//! language: python
//! rule:
//!   pattern: print($MSG)
//! message: "Use logging instead of print"
//! severity: warning
//! fix: "logging.info($MSG)"
//! ```
//!
//! Note: any metavariable referenced in `fix` must be bound by the `pattern`.
//! For languages where a bare expression is not valid at file top level (e.g.
//! Go), use ast-grep's `pattern: { context: ..., selector: ... }` form.
//!
//! ## Rule selection
//!
//! `[lint.astgrep]` accepts poly's uniform rule-selection vocabulary (ADR
//! 0016), same as every other backend: `select` replaces the default active
//! set, `extend_select` adds to it (the way to turn on a rule — built-in or
//! user — that ships `severity: off`), `ignore` removes from it (the way to
//! turn off a rule that is normally on), and `[lint.astgrep.rules.<id>]
//! level` overrides one rule's reported severity. See [`crate::engines::rule_config`] for the
//! shared parser and [`resolve_rules`] for how it is applied here.
//!
//! ## Cache key
//!
//! The engine's `version()` returns a static string embedding the
//! `ast-grep-core` crate version and a marker bumped whenever the engine's
//! own output semantics change (including the built-in pack's content).  The
//! user rules-content hash and the pack's on/off state are injected into
//! `EngineConfig.options` (via `Config::build_astgrep_options`) so that
//! editing a rule file, or toggling `[rules] builtin`, propagates through
//! `serialized_args` into the content-hash cache key without requiring
//! `version()` to change dynamically.

pub mod language;
pub mod map;
pub mod pack;
pub mod rules;
pub mod test;

use std::collections::HashSet;

use ast_grep_config::{CombinedScan, RuleConfig, Severity as AsgSeverity};
use ast_grep_core::tree_sitter::StrDoc;

use super::rule_config::RuleSelection;
use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, SourceFile};
use crate::language::Language;

use language::TslpLanguage;
use map::{diff_to_diagnostic, match_to_diagnostic};
use rules::{RuleMap, load_rules};

/// Version string embedded in the cache key.  Bump whenever the engine's
/// output semantics change independently of the rule files.  Changes to the
/// rule files themselves invalidate the cache via the `rules_hash` folded into
/// `EngineConfig.options` by `Config::build_astgrep_options`.
///
/// `engine-2` marks two such changes: unbound metavariables in `message:` now
/// render as their literal `$NAME` text instead of silently vanishing (see
/// `map::render_message`), and the built-in pack ([`pack`]) is now merged in —
/// both change what a file with no matching user rule can report.
const ENGINE_VERSION: &str = "ast-grep-core-0.45.2-engine-2+tslp1.15.7+builtin-pack-1";

/// Cross-cutting custom-rule engine backed by ast-grep + TSLP grammars.
///
/// Registered once in `registry::engines_for` (after the language-specific
/// engines and before or with typos), and run for every file that has a
/// grammar supported by `tree-sitter-language-pack` and has at least one
/// matching rule — from the built-in pack, a user rule dir, or both.
pub struct AstGrepEngine;

impl Engine for AstGrepEngine {
    fn name(&self) -> &'static str {
        "astgrep"
    }

    /// Returns an empty slice: this engine is cross-cutting and runs for every
    /// language (gated at lint time by whether rules exist for that language).
    fn languages(&self) -> &'static [Language] {
        &[]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: false,
            fix: true,
        }
    }

    fn version(&self) -> &str {
        ENGINE_VERSION
    }

    /// Cross-cutting by registration, but rules are written *per language* —
    /// from the built-in pack, user dirs, or both — so a repo whose only
    /// Kotlin rules are ast-grep rules genuinely has Kotlin lint coverage.
    /// Answered by calling [`resolve_rules`], the exact same lookup
    /// [`Engine::lint`] uses to decide what to scan, rather than the weaker
    /// "some rule source is configured", which would claim coverage of every
    /// language in the repo merely because the (default-on) built-in pack
    /// exists, even for a language the pack has no rules for. The rule maps
    /// backing both sources are cached (user rules by content hash, the pack
    /// forever), so this shares the load the per-file pass is about to do.
    fn provides_language_lint(&self, language: &Language, cfg: &EngineConfig) -> bool {
        let dirs = dirs_from_options(&cfg.options);
        let content_hash = cfg.options.get("rules_hash").and_then(|v| v.as_str()).unwrap_or("");
        let user_rule_map = match load_user_rule_map(&dirs, content_hash) {
            Ok(map) => map,
            Err(_) => return false,
        };
        let (rule_refs, _) = resolve_rules(language.id(), user_rule_map.as_deref(), cfg);
        !rule_refs.is_empty()
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let dirs = dirs_from_options(&cfg.options);
        let content_hash = cfg.options.get("rules_hash").and_then(|v| v.as_str()).unwrap_or("");
        let user_rule_map = load_user_rule_map(&dirs, content_hash)?;

        let lang_name = src.language.id();
        let (rule_refs, selection) = resolve_rules(lang_name, user_rule_map.as_deref(), cfg);
        if rule_refs.is_empty() {
            return Ok(Vec::new());
        }

        let Some(tslp_lang) = TslpLanguage::new(lang_name) else {
            return Ok(Vec::new());
        };

        // `try_new` owns the parse, and `ast-grep-core` already pools the
        // underlying `tree_sitter::Parser` per thread per language behind it
        // (`PARSER_CACHE` in its `tree_sitter` module), so there is nothing for
        // a pool of poly's own to save here.
        let root = match ast_grep_core::AstGrep::<StrDoc<TslpLanguage>>::try_new(&src.content, tslp_lang) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    path = %src.path.display(),
                    error = %e,
                    "ast-grep: failed to parse source; skipping custom-rule lint"
                );
                return Ok(Vec::new());
            }
        };

        let scan = CombinedScan::new(rule_refs);

        let result = scan.scan(&root, true);

        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        for (rule, node_match) in &result.diffs {
            diagnostics.push(diff_to_diagnostic(self.name(), rule, node_match, &selection));
        }

        for (rule, node_matches) in &result.matches {
            for node_match in node_matches {
                diagnostics.push(match_to_diagnostic(self.name(), rule, node_match, &selection));
            }
        }

        pack::apply_noisy_path_exclusions(&src.path, &mut diagnostics);

        Ok(diagnostics)
    }

    fn format(&self, _src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        Ok(FormatOutput::Unchanged)
    }
}

/// Load the user rule map for `dirs`, or `None` when no `[rules] dirs` are
/// configured — kept as a thin wrapper so both trait methods share the exact
/// same "no dirs configured" short-circuit.
fn load_user_rule_map(dirs: &[String], content_hash: &str) -> anyhow::Result<Option<std::sync::Arc<RuleMap>>> {
    if dirs.is_empty() {
        return Ok(None);
    }
    Ok(Some(load_rules(dirs, content_hash)?))
}

/// Whether `[rules] builtin` (`Config::build_astgrep_options`'s
/// `builtin_pack_enabled`) is set. Defaults to `true` when absent — matching
/// `poly_config::RulesConfig`'s own default — so an `EngineConfig` built
/// without going through `Config` (as in unit tests) still gets the pack
/// unless a test opts out explicitly.
fn builtin_pack_enabled(cfg: &EngineConfig) -> bool {
    cfg.options
        .get("builtin_pack_enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// Resolve the active rule set for one language: built-in pack rules (when
/// enabled) with any user rule of the same `id` substituted in, filtered down
/// to the ids `[lint.astgrep]` `select` / `extend_select` / `ignore` leaves
/// active. Shared by [`Engine::lint`] and [`Engine::provides_language_lint`]
/// so "does this language have lint coverage" answers exactly the rules
/// `lint` is about to run.
fn resolve_rules<'a>(
    lang_name: &str,
    user_rule_map: Option<&'a RuleMap>,
    cfg: &EngineConfig,
) -> (Vec<&'a RuleConfig<TslpLanguage>>, RuleSelection) {
    let user_lang_rules = user_rule_map.and_then(|m| m.get(lang_name)).map(Vec::as_slice);
    let user_ids: HashSet<&str> = user_lang_rules
        .map(|rules| rules.iter().map(|r| r.id.as_str()).collect())
        .unwrap_or_default();

    let mut merged: Vec<&'a RuleConfig<TslpLanguage>> = Vec::new();
    if builtin_pack_enabled(cfg)
        && let Some(pack_rules) = pack::builtin_pack().get(lang_name)
    {
        // The pack fills gaps in a repo's own rules; it does not override
        // them, so a user rule sharing a pack rule's id wins outright.
        merged.extend(pack_rules.iter().filter(|r| !user_ids.contains(r.id.as_str())));
    }
    if let Some(user_rules) = user_lang_rules {
        merged.extend(user_rules.iter());
    }

    let selection = RuleSelection::from_options(cfg);
    let keep = active_rule_ids(&merged, &selection);
    merged.retain(|r| keep.contains(r.id.as_str()));
    (merged, selection)
}

/// The set of rule ids that should participate in the scan: every rule whose
/// own YAML declares a severity other than `Off`, minus/plus the
/// `select` / `extend_select` / `ignore` overrides.
///
/// `select` (non-empty) *replaces* the default set outright; `extend_select`
/// adds to whichever set resulted (the way to opt into an `Off` rule);
/// `ignore` removes from it (the way to opt out of an otherwise-active rule).
/// Mirrors `engines::dotenv::skip_checks_from_selection`'s inclusion/exclusion
/// shape, adapted to an id-keyed set instead of a fixed enum.
fn active_rule_ids(rules: &[&RuleConfig<TslpLanguage>], selection: &RuleSelection) -> HashSet<String> {
    let mut keep: HashSet<String> = if selection.select.is_empty() {
        rules
            .iter()
            .filter(|r| !matches!(r.severity, AsgSeverity::Off))
            .map(|r| r.id.clone())
            .collect()
    } else {
        selection.select.iter().cloned().collect()
    };
    for id in &selection.extend_select {
        keep.insert(id.clone());
    }
    for id in &selection.ignore {
        keep.remove(id);
    }
    keep
}

/// Read `rules_dirs` string array from the engine's `options` table.
fn dirs_from_options(options: &toml::Table) -> Vec<String> {
    options
        .get("rules_dirs")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GlobalDefaults;

    fn cfg(options: toml::Table) -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options,
        }
    }

    fn make_src(path: &str, language: Language, content: &str) -> SourceFile {
        SourceFile {
            path: path.into(),
            language,
            content: content.into(),
        }
    }

    /// A `match` whose `Err(_)` arm silently discards the error, used across
    /// these tests to exercise `swallowed-error` — an on-by-default Rust rule
    /// that survived this pack's default-on audit. (`unwrap-used`,
    /// `allow-attribute-without-reason`, and `undocumented-unsafe-block`, the
    /// earlier exemplars, all shipped `off` after that audit; see their YAML
    /// notes.)
    const SWALLOWED_ERROR_SRC: &str =
        "fn f(r: Result<(), ()>) {\n    match r {\n        Ok(_) => {}\n        Err(_) => {}\n    }\n}\n";

    /// With no options at all (mirroring an `EngineConfig` built without going
    /// through `Config`), the built-in pack is still on: a Rust file with an
    /// undocumented `unsafe` block gets flagged even though no `[rules]
    /// dirs`/`select` was ever configured. Reverting `builtin_pack_enabled`'s
    /// default from `true` to `false` (or removing the pack merge in
    /// `resolve_rules`) makes this fail with an empty diagnostics vec.
    #[test]
    fn builtin_pack_fires_by_default_with_no_config() {
        let engine = AstGrepEngine;
        let src = make_src("m.rs", Language::Rust, SWALLOWED_ERROR_SRC);
        let diags = engine.lint(&src, &cfg(toml::Table::new())).unwrap();
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("swallowed-error")),
            "expected the default-on `swallowed-error` built-in rule to fire; got: {diags:?}"
        );
    }

    /// `[rules] builtin = false` (`builtin_pack_enabled = false` in the
    /// resolved options) must silence the pack entirely.
    #[test]
    fn builtin_pack_can_be_disabled() {
        let engine = AstGrepEngine;
        let mut options = toml::Table::new();
        options.insert("builtin_pack_enabled".to_string(), toml::Value::Boolean(false));
        let src = make_src("m.rs", Language::Rust, SWALLOWED_ERROR_SRC);
        let diags = engine.lint(&src, &cfg(options)).unwrap();
        assert!(
            diags.is_empty(),
            "builtin_pack_enabled = false must disable the pack; got: {diags:?}"
        );
    }

    /// `extend_select` turns on a built-in rule that ships `severity: off`
    /// (opt-in) — here `todo-marker`, which fires on a `// TODO` comment.
    #[test]
    fn extend_select_enables_an_off_rule() {
        let engine = AstGrepEngine;
        let mut options = toml::Table::new();
        options.insert(
            "extend_select".to_string(),
            toml::Value::Array(vec![toml::Value::String("todo-marker".to_string())]),
        );
        let src = make_src("m.rs", Language::Rust, "// TODO: fix this\nfn f() {}\n");
        let diags = engine.lint(&src, &cfg(options)).unwrap();
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("todo-marker")),
            "extend_select must enable the off-by-default todo-marker rule; got: {diags:?}"
        );
    }

    /// Without `extend_select`, the same off-by-default rule stays silent.
    #[test]
    fn off_rule_stays_silent_without_extend_select() {
        let engine = AstGrepEngine;
        let src = make_src("m.rs", Language::Rust, "// TODO: fix this\nfn f() {}\n");
        let diags = engine.lint(&src, &cfg(toml::Table::new())).unwrap();
        assert!(
            !diags.iter().any(|d| d.code.as_deref() == Some("todo-marker")),
            "todo-marker ships `severity: off` and must not fire unselected; got: {diags:?}"
        );
    }

    /// `ignore` turns off a built-in rule that is normally on by default.
    #[test]
    fn ignore_disables_an_on_rule() {
        let engine = AstGrepEngine;
        let mut options = toml::Table::new();
        options.insert(
            "ignore".to_string(),
            toml::Value::Array(vec![toml::Value::String("swallowed-error".to_string())]),
        );
        let src = make_src("m.rs", Language::Rust, SWALLOWED_ERROR_SRC);
        let diags = engine.lint(&src, &cfg(options)).unwrap();
        assert!(
            !diags.iter().any(|d| d.code.as_deref() == Some("swallowed-error")),
            "ignore must disable the on-by-default swallowed-error rule; got: {diags:?}"
        );
    }

    /// `[lint.astgrep.rules.<id>] level` overrides a fired diagnostic's
    /// severity, same vocabulary as every other backend (ADR 0016).
    #[test]
    fn rule_level_override_changes_severity() {
        let engine = AstGrepEngine;
        let mut rules_table = toml::Table::new();
        let mut rule_opts = toml::Table::new();
        rule_opts.insert("level".to_string(), toml::Value::String("error".to_string()));
        rules_table.insert("swallowed-error".to_string(), toml::Value::Table(rule_opts));
        let mut options = toml::Table::new();
        options.insert("rules".to_string(), toml::Value::Table(rules_table));
        let src = make_src("m.rs", Language::Rust, SWALLOWED_ERROR_SRC);
        let diags = engine.lint(&src, &cfg(options)).unwrap();
        let hit = diags
            .iter()
            .find(|d| d.code.as_deref() == Some("swallowed-error"))
            .unwrap_or_else(|| panic!("expected swallowed-error; got: {diags:?}"));
        assert_eq!(hit.severity, crate::engine::Severity::Error);
    }

    /// `provides_language_lint` must agree with `lint`: Rust has built-in
    /// coverage by default, but a language the pack has no rules for (and no
    /// user dir provides either) must not falsely claim coverage merely
    /// because the (enabled) pack exists for *other* languages.
    #[test]
    fn provides_language_lint_matches_actual_coverage() {
        let engine = AstGrepEngine;
        let c = cfg(toml::Table::new());
        assert!(engine.provides_language_lint(&Language::Rust, &c));
        assert!(!engine.provides_language_lint(&Language::Other("cobol".to_string()), &c));
    }

    /// A user rule sharing a built-in rule's id replaces it outright — the
    /// pack fills gaps, it does not override user intent. A user's own
    /// `swallowed-error` rule with a distinct pattern (and a
    /// distinct message) must be the one that fires, not the pack's.
    #[test]
    fn user_rule_overrides_builtin_rule_of_same_id() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("swallowed-error.yml"),
            "id: swallowed-error\nlanguage: rust\nseverity: warning\nmessage: custom override message\nrule:\n  pattern: totally_custom_pattern()\n",
        )
        .unwrap();
        let dirs = vec![dir.path().to_string_lossy().into_owned()];
        let hash = rules::rules_hash(&dirs);
        let mut options = toml::Table::new();
        options.insert(
            "rules_dirs".to_string(),
            toml::Value::Array(dirs.iter().cloned().map(toml::Value::String).collect()),
        );
        options.insert("rules_hash".to_string(), toml::Value::String(hash));

        let engine = AstGrepEngine;
        // The pack's own swallowed-error pattern (`Err(_) => {}`) must NOT fire
        // (proves the user's rule replaced it, rather than both running).
        let src = make_src("m.rs", Language::Rust, SWALLOWED_ERROR_SRC);
        let diags = engine.lint(&src, &cfg(options)).unwrap();
        assert!(
            !diags.iter().any(|d| d.code.as_deref() == Some("swallowed-error")),
            "user's swallowed-error rule (which doesn't match `Err(_) => {{}}`) must \
             fully replace the built-in one, not run alongside it; got: {diags:?}"
        );
    }
}
