//! Custom-rule lint + autofix engine built on `ast-grep-core`.
//!
//! [`AstGrepEngine`] is a cross-cutting backend (registered after all
//! language-specific engines, similar to [`crate::engines::typos::TyposEngine`])
//! that loads user-authored YAML rule packs from the configured `[rules] dirs`
//! directories and runs them against every file whose language has matching
//! rules.
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
//! ## Cache key
//!
//! The engine's `version()` returns a static string embedding the
//! `ast-grep-core` crate version.  The rules-content hash is injected into
//! `EngineConfig.options` (via `Config::build_astgrep_options`) so that any
//! edit to a rule file propagates through `serialized_args` into the
//! content-hash cache key without requiring `version()` to change dynamically.

pub mod language;
pub mod map;
pub mod rules;
pub mod test;

use ast_grep_config::{CombinedScan, Severity as AsgSeverity};
use ast_grep_core::tree_sitter::StrDoc;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, SourceFile};
use crate::language::Language;

use language::TslpLanguage;
use map::{diff_to_diagnostic, match_to_diagnostic};
use rules::load_rules;

/// Version string embedded in the cache key.  Bump whenever the engine's
/// output semantics change independently of the rule files.  Changes to the
/// rule files themselves invalidate the cache via the `rules_hash` folded into
/// `EngineConfig.options` by `Config::build_astgrep_options`.
const ENGINE_VERSION: &str = "ast-grep-core-0.44.1-engine-1+tslp1.13.2";

/// Cross-cutting custom-rule engine backed by ast-grep + TSLP grammars.
///
/// Registered once in `registry::engines_for` (after the language-specific
/// engines and before or with typos), and run for every file that has a
/// grammar supported by `tree-sitter-language-pack` and has at least one
/// matching rule in the configured rule packs.
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

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let dirs = dirs_from_options(&cfg.options);
        if dirs.is_empty() {
            return Ok(Vec::new());
        }

        let content_hash = cfg.options.get("rules_hash").and_then(|v| v.as_str()).unwrap_or("");
        let rule_map = load_rules(&dirs, content_hash)?;

        let lang_name = src.language.id();
        let Some(lang_rules) = rule_map.get(lang_name) else {
            return Ok(Vec::new());
        };
        if lang_rules.is_empty() {
            return Ok(Vec::new());
        }

        let Some(tslp_lang) = TslpLanguage::new(lang_name) else {
            return Ok(Vec::new());
        };

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

        let rule_refs: Vec<_> = lang_rules
            .iter()
            .filter(|r| !matches!(r.severity, AsgSeverity::Off))
            .collect();
        if rule_refs.is_empty() {
            return Ok(Vec::new());
        }
        let scan = CombinedScan::new(rule_refs);

        let result = scan.scan(&root, true);

        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        for (rule, node_match) in &result.diffs {
            diagnostics.push(diff_to_diagnostic(self.name(), rule, node_match));
        }

        for (rule, node_matches) in &result.matches {
            for node_match in node_matches {
                diagnostics.push(match_to_diagnostic(self.name(), rule, node_match));
            }
        }

        Ok(diagnostics)
    }

    fn format(&self, _src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        Ok(FormatOutput::Unchanged)
    }
}

/// Read `rules_dirs` string array from the engine's `options` table.
fn dirs_from_options(options: &toml::Table) -> Vec<String> {
    options
        .get("rules_dirs")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
        .unwrap_or_default()
}
