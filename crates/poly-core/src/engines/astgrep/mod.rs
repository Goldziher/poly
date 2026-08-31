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
//! layering, and the path-exclusion problem for a handful of noisy rules), and
//! [`exclusions`] for the per-rule `ignores:` globs that solve it — collected
//! here, applied by the runner, and overridable from `[per-file-ignores]`.
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

pub(crate) mod exclusions;
pub mod language;
pub mod map;
pub mod pack;
pub mod rules;
pub mod test;

use std::collections::{BTreeSet, HashSet};

use ast_grep_config::{CombinedScan, RuleConfig, Severity as AsgSeverity};
use ast_grep_core::tree_sitter::StrDoc;
use serde::Serialize;

use super::rule_config::RuleSelection;
use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, OptionKeys, OptionTable, Severity, SourceFile};
use crate::language::Language;

use language::{TslpLanguage, grammar_for_language_id};
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
///
/// `builtin-pack-2` marks a third: the pack's default path exclusions used to
/// be applied *inside* this engine, so the cached payload was already
/// filtered. They are now declared as `ignores:` in each rule's own YAML and
/// applied by the runner alongside `[per-file-ignores]` (see
/// [`exclusions`]), which means this engine now returns the **unfiltered**
/// diagnostics for a path a pack rule excludes. A cache written by the old
/// binary would serve the filtered set as if it were the raw one, and the
/// run's `suppressed` list would then be empty on a file that did suppress.
///
/// `engine-3` marks a fourth: a file's language is now resolved to the grammar
/// that parses it (`language::grammar_for_language_id`) before rules are looked
/// up, so five languages whose poly id is not a grammar name — `jsx`, `jsonc`,
/// `mdx`, `jinja`, `mustache` — reach rules for the first time. A cache written
/// by the old binary holds the empty result those files used to get.
const ENGINE_VERSION: &str = "ast-grep-core-0.45.2-engine-3+tslp1.15.12+builtin-pack-2";

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

    /// Rule selection, and only from the language-agnostic `[lint.astgrep]`
    /// table: `Config::engine_config` builds this backend's options without
    /// consulting `[lint.<lang>.astgrep]` at all, so every key written there is
    /// dead and is reported as such. The rule *sources* live in the top-level
    /// `[rules]` table, not here.
    fn option_keys(&self, table: OptionTable) -> OptionKeys {
        match table {
            OptionTable::CrossCuttingLint => OptionKeys::declared(&[]).with_rule_selection(),
            OptionTable::Lint => OptionKeys::declared(&[]).with_note(
                "ast-grep is configured in the language-agnostic `[lint.astgrep]` table; \
                 a per-language `[lint.<lang>.astgrep]` table is never read.",
            ),
            OptionTable::Format => OptionKeys::declared(&[]),
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
        let (rule_refs, _) = resolve_rules(
            grammar_for_language_id(language.id()),
            user_rule_map.as_deref(),
            cfg,
        );
        !rule_refs.is_empty()
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let dirs = dirs_from_options(&cfg.options);
        let content_hash = cfg.options.get("rules_hash").and_then(|v| v.as_str()).unwrap_or("");
        let user_rule_map = load_user_rule_map(&dirs, content_hash)?;

        let lang_name = grammar_for_language_id(src.language.id());
        let (rule_refs, selection) = resolve_rules(lang_name, user_rule_map.as_deref(), cfg);
        let Some(first_rule) = rule_refs.first() else {
            return Ok(Vec::new());
        };

        // The grammar comes from a rule rather than from a second
        // `TslpLanguage::new(lang_name)` lookup. Every rule in `rule_refs` is
        // keyed under `lang_name` *because* its own `language:` deserialized to
        // that already-validated grammar, so this cannot fail — where the
        // lookup could, silently returning no diagnostics for a file poly had
        // just counted as linted.
        let tslp_lang = first_rule.language.clone();

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
    let mut merged = merge_rules(lang_name, user_rule_map, cfg);
    let selection = RuleSelection::from_options(cfg);
    let keep = active_rule_ids(&merged, &selection);
    merged.retain(|r| keep.contains(r.id.as_str()));
    (merged, selection)
}

/// The merged, *unfiltered* rule set for one language: built-in pack rules
/// (when enabled) with any user rule of the same `id` substituted in.
///
/// Split out of [`resolve_rules`] so [`list_rules`] can report the rules
/// `select` / `extend_select` / `ignore` turned **off** — which
/// [`resolve_rules`] has already dropped — without restating the merge.
fn merge_rules<'a>(
    lang_name: &str,
    user_rule_map: Option<&'a RuleMap>,
    cfg: &EngineConfig,
) -> Vec<&'a RuleConfig<TslpLanguage>> {
    let user_lang_rules = user_rule_map.and_then(|m| m.get(lang_name)).map(Vec::as_slice);
    let user_ids = user_rule_ids(user_rule_map, lang_name);

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
    merged
}

/// The ids of the user's own rules for `lang_name` — the ids that displace a
/// pack rule in [`merge_rules`], and therefore also the ids [`list_rules`]
/// reports as [`RuleSource::User`].
fn user_rule_ids<'a>(user_rule_map: Option<&'a RuleMap>, lang_name: &str) -> HashSet<&'a str> {
    user_rule_map
        .and_then(|m| m.get(lang_name))
        .map(|rules| rules.iter().map(|r| r.id.as_str()).collect())
        .unwrap_or_default()
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

/// Where a listed rule came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleSource {
    /// A rule from poly's built-in pack ([`pack`]), embedded in the binary.
    Builtin,
    /// A user-authored rule loaded from a `[rules] dirs` directory.
    User,
}

impl RuleSource {
    /// Lowercase name (`builtin` / `user`), matching the serialized form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::User => "user",
        }
    }
}

/// One rule as the engine would resolve it for a given config — the row behind
/// `poly rules list` and the MCP `rules` tool.
#[derive(Debug, Clone, Serialize)]
pub struct RuleListing {
    /// The rule's `id`, as reported in a diagnostic's `code`.
    pub id: String,
    /// The language name the rule targets (`rust`, `swift`, …).
    pub language: String,
    /// Built-in pack rule, or user-authored rule.
    pub source: RuleSource,
    /// The severity the rule's own YAML declares: `error`, `warning`, `info`,
    /// `hint`, or `off` for a rule that ships opt-in.
    pub default_severity: &'static str,
    /// The severity a finding is reported at under this config, or `off` when
    /// the rule does not run at all.
    pub severity: &'static str,
    /// Whether the rule participates in a scan under this config.
    pub enabled: bool,
}

/// Every ast-grep rule this config resolves to, built-in pack and user rules
/// alike, whether or not each one is currently active.
///
/// The merge is `merge_rules` and the on/off decision is `active_rule_ids` —
/// the exact functions `resolve_rules` uses — so a listing cannot drift from
/// what [`Engine::lint`] runs. `dirs` are the user rule directories (the CLI
/// lets a caller override `[rules] dirs`), and `cfg` supplies `[rules] builtin`
/// plus the `[lint.astgrep]` selection keys.
pub fn list_rules(dirs: &[String], cfg: &EngineConfig) -> anyhow::Result<Vec<RuleListing>> {
    let user_rule_map = load_user_rule_map(dirs, &rules::rules_hash(dirs))?;
    let user_rule_map = user_rule_map.as_deref();

    let mut languages: BTreeSet<&str> = BTreeSet::new();
    if builtin_pack_enabled(cfg) {
        languages.extend(pack::builtin_pack().keys().map(String::as_str));
    }
    if let Some(map) = user_rule_map {
        languages.extend(map.keys().map(String::as_str));
    }

    let selection = RuleSelection::from_options(cfg);
    let mut listings = Vec::new();
    for language in languages {
        let merged = merge_rules(language, user_rule_map, cfg);
        let active = active_rule_ids(&merged, &selection);
        let user_ids = user_rule_ids(user_rule_map, language);
        for rule in merged {
            let enabled = active.contains(rule.id.as_str());
            listings.push(RuleListing {
                id: rule.id.clone(),
                language: language.to_string(),
                source: if user_ids.contains(rule.id.as_str()) {
                    RuleSource::User
                } else {
                    RuleSource::Builtin
                },
                default_severity: declared_severity_name(&rule.severity),
                severity: effective_severity_name(rule, &selection, enabled),
                enabled,
            });
        }
    }
    listings.sort_by(|a, b| (&a.language, &a.id).cmp(&(&b.language, &b.id)));
    Ok(listings)
}

/// A rule's own declared severity as a lowercase name — `off` included, which
/// is why this is not [`Severity`] (poly's `Severity` has no "off" state; an
/// off rule simply never reports).
fn declared_severity_name(severity: &AsgSeverity) -> &'static str {
    match severity {
        AsgSeverity::Error => "error",
        AsgSeverity::Warning => "warning",
        AsgSeverity::Info => "info",
        AsgSeverity::Hint => "hint",
        AsgSeverity::Off => "off",
    }
}

/// A poly [`Severity`] as a lowercase name, matching its serde representation.
fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
        Severity::Hint => "hint",
    }
}

/// The severity a finding from `rule` would carry under `selection`, or `off`
/// when the rule is not active.
///
/// Mirrors `map::resolve_severity`, which is the authority for what a real
/// diagnostic gets: an explicit `[lint.astgrep.rules.<id>] level` wins, and a
/// rule declared `off` that was nonetheless selected reports at `warning`. The
/// `listing_severity_matches_the_reported_diagnostic` test pins the two
/// together so this cannot drift.
fn effective_severity_name(rule: &RuleConfig<TslpLanguage>, selection: &RuleSelection, enabled: bool) -> &'static str {
    if !enabled {
        return "off";
    }
    if let Some(level) = selection.rules.get(&rule.id).and_then(|opts| opts.level) {
        return severity_name(level);
    }
    if matches!(rule.severity, AsgSeverity::Off) {
        return "warning";
    }
    declared_severity_name(&rule.severity)
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
    // ── `list_rules`: the rule-listing surface (`poly rules list`, MCP `rules`) ──

    /// Look up one listed rule by (language, id) — an id alone is ambiguous,
    /// since the pack ships `todo-marker` once per language.
    fn listed<'a>(listings: &'a [RuleListing], language: &str, id: &str) -> &'a RuleListing {
        listings
            .iter()
            .find(|listing| listing.language == language && listing.id == id)
            .unwrap_or_else(|| panic!("expected {language}/{id} in the listing; got: {listings:?}"))
    }

    /// An options table pointing at a temp dir holding one user rule file.
    fn user_rule_options(dir: &std::path::Path) -> (Vec<String>, toml::Table) {
        let dirs = vec![dir.to_string_lossy().into_owned()];
        let mut options = toml::Table::new();
        options.insert(
            "rules_dirs".to_string(),
            toml::Value::Array(dirs.iter().cloned().map(toml::Value::String).collect()),
        );
        options.insert("rules_hash".to_string(), toml::Value::String(rules::rules_hash(&dirs)));
        (dirs, options)
    }

    /// The built-in pack is listed with no config at all — the defect this
    /// listing exists to close was a user who could not find out where a
    /// `force-cast` warning came from.
    #[test]
    fn list_rules_includes_builtin_pack_rules() {
        let listings = list_rules(&[], &cfg(toml::Table::new())).unwrap();
        let rule = listed(&listings, "rust", "swallowed-error");
        assert_eq!(rule.source, RuleSource::Builtin);
        assert_eq!(rule.default_severity, "warning");
        assert_eq!(rule.severity, "warning");
        assert!(rule.enabled, "swallowed-error ships on by default");
        assert_eq!(listed(&listings, "swift", "force-cast").source, RuleSource::Builtin);
        assert_eq!(
            listings.len(),
            26,
            "every pack rule must be listed, not only the active ones"
        );
    }

    /// An opt-in (`severity: off`) rule is listed rather than hidden, and is
    /// marked off in both the declared and the effective column.
    #[test]
    fn list_rules_lists_an_off_rule_and_marks_it_off() {
        let listings = list_rules(&[], &cfg(toml::Table::new())).unwrap();
        let rule = listed(&listings, "rust", "todo-marker");
        assert_eq!(rule.default_severity, "off");
        assert_eq!(rule.severity, "off");
        assert!(!rule.enabled);
    }

    /// `[rules] builtin = false` removes the pack from the listing, exactly as
    /// it removes it from a scan.
    #[test]
    fn list_rules_omits_the_pack_when_builtin_is_disabled() {
        let mut options = toml::Table::new();
        options.insert("builtin_pack_enabled".to_string(), toml::Value::Boolean(false));
        let listings = list_rules(&[], &cfg(options)).unwrap();
        assert!(
            listings.is_empty(),
            "builtin = false with no user dirs must list nothing; got: {listings:?}"
        );
    }

    /// A user rule sharing a pack rule's id is listed **once**, as the user's —
    /// the merge semantics `resolve_rules` applies, not two rows.
    #[test]
    fn list_rules_shows_a_user_override_once_and_as_the_users() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("swallowed-error.yml"),
            "id: swallowed-error\nlanguage: rust\nseverity: error\nmessage: custom\nrule:\n  pattern: custom_pattern()\n",
        )
        .unwrap();
        let (dirs, options) = user_rule_options(dir.path());

        let listings = list_rules(&dirs, &cfg(options)).unwrap();
        let matching: Vec<&RuleListing> = listings
            .iter()
            .filter(|listing| listing.id == "swallowed-error")
            .collect();
        assert_eq!(matching.len(), 1, "one row per id, not one per source: {matching:?}");
        assert_eq!(matching[0].source, RuleSource::User);
        assert_eq!(
            matching[0].default_severity, "error",
            "the user's severity, not the pack's"
        );
    }

    /// `ignore` moves an on-by-default pack rule to off in the listing.
    #[test]
    fn list_rules_reflects_ignore() {
        let mut options = toml::Table::new();
        options.insert(
            "ignore".to_string(),
            toml::Value::Array(vec![toml::Value::String("swallowed-error".to_string())]),
        );
        let listings = list_rules(&[], &cfg(options)).unwrap();
        let rule = listed(&listings, "rust", "swallowed-error");
        assert!(!rule.enabled, "ignored rule must not read as enabled");
        assert_eq!(rule.severity, "off");
        assert_eq!(rule.default_severity, "warning", "the declared default is unchanged");
    }

    /// `extend_select` moves an off-by-default pack rule to on, reported at the
    /// `warning` an opted-in rule actually fires at.
    #[test]
    fn list_rules_reflects_extend_select() {
        let mut options = toml::Table::new();
        options.insert(
            "extend_select".to_string(),
            toml::Value::Array(vec![toml::Value::String("todo-marker".to_string())]),
        );
        let listings = list_rules(&[], &cfg(options)).unwrap();
        let rule = listed(&listings, "rust", "todo-marker");
        assert!(rule.enabled);
        assert_eq!(rule.severity, "warning");
        assert_eq!(rule.default_severity, "off");
    }

    /// `select` replaces the default active set outright.
    #[test]
    fn list_rules_reflects_select_replacing_the_default_set() {
        let mut options = toml::Table::new();
        options.insert(
            "select".to_string(),
            toml::Value::Array(vec![toml::Value::String("todo-marker".to_string())]),
        );
        let listings = list_rules(&[], &cfg(options)).unwrap();
        assert!(listed(&listings, "rust", "todo-marker").enabled);
        assert!(
            !listed(&listings, "rust", "swallowed-error").enabled,
            "select replaces the default set, so an unselected on-by-default rule is off"
        );
    }

    /// `[lint.astgrep.rules.<id>] level` moves a rule's reported severity.
    #[test]
    fn list_rules_reflects_a_level_override() {
        let mut rule_opts = toml::Table::new();
        rule_opts.insert("level".to_string(), toml::Value::String("error".to_string()));
        let mut rules_table = toml::Table::new();
        rules_table.insert("swallowed-error".to_string(), toml::Value::Table(rule_opts));
        let mut options = toml::Table::new();
        options.insert("rules".to_string(), toml::Value::Table(rules_table));

        let listings = list_rules(&[], &cfg(options)).unwrap();
        let rule = listed(&listings, "rust", "swallowed-error");
        assert_eq!(rule.severity, "error", "the level override is the effective severity");
        assert_eq!(rule.default_severity, "warning");
    }

    /// The listing's effective severity must equal the severity a real
    /// diagnostic carries. `effective_severity_name` mirrors
    /// `map::resolve_severity` (a private fn in another module); this is the
    /// guard that keeps the two from drifting apart.
    #[test]
    fn listing_severity_matches_the_reported_diagnostic() {
        let engine = AstGrepEngine;

        let mut level_opts = toml::Table::new();
        let mut rule_opts = toml::Table::new();
        rule_opts.insert("level".to_string(), toml::Value::String("info".to_string()));
        let mut rules_table = toml::Table::new();
        rules_table.insert("swallowed-error".to_string(), toml::Value::Table(rule_opts));
        level_opts.insert("rules".to_string(), toml::Value::Table(rules_table));

        let mut opt_in_opts = toml::Table::new();
        opt_in_opts.insert(
            "extend_select".to_string(),
            toml::Value::Array(vec![toml::Value::String("todo-marker".to_string())]),
        );

        for (options, id, source) in [
            (toml::Table::new(), "swallowed-error", SWALLOWED_ERROR_SRC),
            (level_opts, "swallowed-error", SWALLOWED_ERROR_SRC),
            (opt_in_opts, "todo-marker", "// TODO: fix this\nfn f() {}\n"),
        ] {
            let config = cfg(options);
            let listing = list_rules(&[], &config).unwrap();
            let listed_rule = listed(&listing, "rust", id);
            let src = make_src("m.rs", Language::Rust, source);
            let diags = engine.lint(&src, &config).unwrap();
            let diagnostic = diags
                .iter()
                .find(|d| d.code.as_deref() == Some(id))
                .unwrap_or_else(|| panic!("expected {id} to fire; got: {diags:?}"));
            assert_eq!(
                listed_rule.severity,
                severity_name(diagnostic.severity),
                "listing severity for {id} must match the reported diagnostic"
            );
        }
    }
}
