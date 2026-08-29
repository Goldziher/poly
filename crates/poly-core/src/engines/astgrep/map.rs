//! Map ast-grep scan results to poly [`Diagnostic`]s.
//!
//! Two categories arrive from `CombinedScan::scan`:
//! - `diffs` — fixable matches: emit a `Diagnostic` with a non-empty `fix` vec
//!   built from the `NodeMatch`'s byte range and the rule's `Fixer`.
//! - `matches` — lint-only matches: emit a `Diagnostic` with an empty `fix` vec.

use ast_grep_config::{RuleConfig, Severity as AsgSeverity};
use ast_grep_core::language::Language as AsgLanguage;
use ast_grep_core::meta_var::MetaVarEnv;
use ast_grep_core::tree_sitter::StrDoc;
use ast_grep_core::{Node, NodeMatch};

use crate::engine::{Diagnostic, Edit, Severity, Span};
use crate::engines::rule_config::RuleSelection;

use super::language::TslpLanguage;

/// Convert a fixable ast-grep diff (rule + matched node) to a [`Diagnostic`]
/// carrying the byte-range autofix edits.
pub fn diff_to_diagnostic(
    engine_name: &str,
    rule: &RuleConfig<TslpLanguage>,
    node_match: &NodeMatch<'_, StrDoc<TslpLanguage>>,
    selection: &RuleSelection,
) -> Diagnostic {
    let fixes = fix_edits(rule, node_match);
    build_diagnostic(engine_name, rule, node_match, fixes, selection)
}

/// Build the byte-range autofix [`Edit`]s for a matched node from the rule's
/// `Fixer`. Empty when the rule declares no `fix`. Shared by
/// [`diff_to_diagnostic`] and the rule-test runner so the CLI-visible fix and
/// the tested fix come from one code path.
pub fn fix_edits(rule: &RuleConfig<TslpLanguage>, node_match: &NodeMatch<'_, StrDoc<TslpLanguage>>) -> Vec<Edit> {
    rule.fixer
        .iter()
        .map(|fixer| {
            let edit = node_match.make_edit(&rule.matcher, fixer);
            Edit {
                start_byte: edit.position,
                end_byte: edit.position + edit.deleted_length,
                replacement: String::from_utf8_lossy(&edit.inserted_text).into_owned(),
            }
        })
        .collect()
}

/// Convert a lint-only ast-grep match (rule + matched nodes) to a
/// [`Diagnostic`] with no fix edits.
pub fn match_to_diagnostic(
    engine_name: &str,
    rule: &RuleConfig<TslpLanguage>,
    node_match: &NodeMatch<'_, StrDoc<TslpLanguage>>,
    selection: &RuleSelection,
) -> Diagnostic {
    build_diagnostic(engine_name, rule, node_match, Vec::new(), selection)
}

fn build_diagnostic(
    engine_name: &str,
    rule: &RuleConfig<TslpLanguage>,
    node_match: &NodeMatch<'_, StrDoc<TslpLanguage>>,
    fix: Vec<Edit>,
    selection: &RuleSelection,
) -> Diagnostic {
    let span = {
        let start = node_match.start_pos();
        let end = node_match.end_pos();
        Span {
            start_line: (start.line() + 1) as u32,
            start_col: (start.column(node_match) + 1) as u32,
            end_line: (end.line() + 1) as u32,
            end_col: (end.column(node_match) + 1) as u32,
        }
    };

    let message = render_message(rule, node_match);
    let severity = resolve_severity(rule, selection);

    Diagnostic {
        engine: engine_name.to_string(),
        code: Some(rule.id.clone()),
        severity,
        title: message,
        description: rule.note.as_deref().map(str::to_string),
        span: Some(span),
        url: rule.url.as_deref().map(str::to_string),
        fix,
        metadata: std::collections::BTreeMap::new(),
    }
}

/// Resolve a diagnostic's severity.
///
/// An explicit `[lint.astgrep.rules.<id>] level` always wins (ADR 0016's
/// uniform per-rule override). Otherwise, a rule whose own YAML declares
/// `severity: off` only reaches this point because `select`/`extend_select`
/// opted it in — see `active_rule_ids` in `mod.rs`; an `Off` rule that was not
/// selected never enters the scan — so it is reported at `Warning` rather
/// than the `Hint` [`map_severity`] would otherwise give an `Off` rule: a rule
/// a user explicitly turned on is a real finding, not a hint the engine is
/// merely allowed to mention.
fn resolve_severity(rule: &RuleConfig<TslpLanguage>, selection: &RuleSelection) -> Severity {
    if let Some(level) = selection.rules.get(&rule.id).and_then(|opts| opts.level) {
        return level;
    }
    if matches!(rule.severity, AsgSeverity::Off) {
        return Severity::Warning;
    }
    map_severity(&rule.severity)
}

/// Map ast-grep `Severity` to poly `Severity`.
fn map_severity(s: &AsgSeverity) -> Severity {
    match s {
        AsgSeverity::Error => Severity::Error,
        AsgSeverity::Warning => Severity::Warning,
        AsgSeverity::Info => Severity::Info,
        AsgSeverity::Hint | AsgSeverity::Off => Severity::Hint,
    }
}

/// Render `rule.message` for a match, preserving an unbound metavariable's
/// literal `$NAME` text instead of `RuleConfig::get_message`'s default
/// behaviour of silently deleting it.
///
/// `rule.get_message` (`ast-grep-config`) treats every `$NAME` in the message
/// template as a variable reference and drops it from the rendered output
/// when it never bound for this match — e.g. a name captured by only one arm
/// of an `any:` rule. A message written as `"defer $X.Close() in a loop"`
/// then renders as `"defer .Close()"`, with no trace anything was missing.
///
/// None of poly's shipped rules reference a metavariable in `message:` today
/// (every built-in rule's message is static text — see
/// `crates/poly-core/src/engines/astgrep/builtin/**`), so this only changes
/// behaviour for a rule — built-in or user-authored — that starts doing so;
/// the common case below (every reference bound) still returns
/// `rule.get_message`'s own rendering unchanged.
fn render_message(rule: &RuleConfig<TslpLanguage>, node_match: &NodeMatch<'_, StrDoc<TslpLanguage>>) -> String {
    let refs = scan_meta_var_refs(&rule.message, rule.language.meta_var_char());
    let env = node_match.get_env();
    if refs.iter().all(|r| r.is_bound(env)) {
        return rule.get_message(node_match);
    }
    render_with_fallback(&rule.message, &refs, env)
}

/// One `$NAME` / `$$$NAME` reference found in a message template.
struct MetaVarRef {
    /// Byte offset of the reference's first `$` in the template.
    start: usize,
    /// Byte offset just past the reference's name.
    end: usize,
    /// `true` for a `$$$NAME` (multi-node capture) reference.
    is_multi: bool,
    name: String,
}

impl MetaVarRef {
    fn is_bound(&self, env: &MetaVarEnv<'_, StrDoc<TslpLanguage>>) -> bool {
        if self.is_multi {
            !env.get_multiple_matches(&self.name).is_empty()
        } else {
            env.get_match(&self.name).is_some()
        }
    }
}

/// A metavariable name's characters: uppercase ASCII letters, digits, or `_`.
/// Mirrors `ast-grep-core`'s own (private) `is_valid_meta_var_char`.
fn is_valid_meta_var_char(c: char) -> bool {
    c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()
}

/// Find every `$NAME` / `$$$NAME` reference in `template`, mirroring
/// `ast-grep-core`'s own template scan closely enough to agree on which spans
/// are references: a run of exactly one or three `meta_char` bytes followed
/// by one or more valid name characters. A run of exactly two is a documented
/// ast-grep edge case (`ast-grep-core::replacer::split_first_meta_var`) with
/// no defined poly rule relying on it, so it is left as literal text here
/// rather than replicated.
fn scan_meta_var_refs(template: &str, meta_char: char) -> Vec<MetaVarRef> {
    let Ok(meta_byte) = u8::try_from(u32::from(meta_char)) else {
        // Every TSLP language poly ships today uses the trait default ('$'),
        // so this never triggers; a future non-ASCII meta_var_char just skips
        // this fallback rather than mis-scanning multi-byte UTF-8 as if it
        // were single ASCII bytes.
        return Vec::new();
    };
    let bytes = template.as_bytes();
    let mut out = Vec::new();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] != meta_byte {
            idx += 1;
            continue;
        }
        let start = idx;
        let mut count = 0u8;
        while idx < bytes.len() && bytes[idx] == meta_byte && count < 3 {
            count += 1;
            idx += 1;
        }
        if count == 2 {
            continue;
        }
        let name_start = idx;
        while idx < bytes.len() && is_valid_meta_var_char(bytes[idx] as char) {
            idx += 1;
        }
        if idx == name_start {
            continue;
        }
        out.push(MetaVarRef {
            start,
            end: idx,
            is_multi: count == 3,
            name: template[name_start..idx].to_string(),
        });
    }
    out
}

/// Render `template`, substituting each bound reference with its matched
/// text and leaving each unbound reference as the literal text it appeared
/// as. Used only when at least one reference is unbound — see
/// [`render_message`]; the common (fully-bound) case uses ast-grep's own
/// `get_message` unchanged, including its indent-aware multi-line handling
/// that this fallback does not attempt to replicate.
fn render_with_fallback(template: &str, refs: &[MetaVarRef], env: &MetaVarEnv<'_, StrDoc<TslpLanguage>>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut last = 0;
    for r in refs {
        out.push_str(&template[last..r.start]);
        if r.is_bound(env) {
            out.push_str(&bound_text(r, env));
        } else {
            out.push_str(&template[r.start..r.end]);
        }
        last = r.end;
    }
    out.push_str(&template[last..]);
    out
}

/// The source text a bound reference resolves to: a single node's text, or
/// every captured node's text space-joined for a `$$$NAME` multi-capture.
fn bound_text(r: &MetaVarRef, env: &MetaVarEnv<'_, StrDoc<TslpLanguage>>) -> String {
    if r.is_multi {
        env.get_multiple_matches(&r.name)
            .iter()
            .map(Node::text)
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        env.get_match(&r.name)
            .map(|n| n.text().into_owned())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use ast_grep_config::{CombinedScan, GlobalRules, from_yaml_string};
    use ast_grep_core::AstGrep;

    use super::*;

    fn parse_rule(yaml: &str) -> RuleConfig<TslpLanguage> {
        let globals = GlobalRules::default();
        let mut rules: Vec<RuleConfig<TslpLanguage>> = from_yaml_string(yaml, &globals).expect("valid rule YAML");
        rules.remove(0)
    }

    /// Render the message for the first match of `rule` against `code`
    /// (Python), going through the real `match_to_diagnostic` path.
    fn first_match_title(rule: &RuleConfig<TslpLanguage>, code: &str) -> String {
        let lang = TslpLanguage::new("python").expect("python is a known TSLP grammar");
        let root = AstGrep::<StrDoc<TslpLanguage>>::try_new(code, lang).expect("valid python source");
        let scan = CombinedScan::new(vec![rule]);
        let result = scan.scan(&root, false);
        let (_, node_matches) = result.matches.first().expect("expected the rule to match");
        let node_match = &node_matches[0];
        match_to_diagnostic("astgrep", rule, node_match, &RuleSelection::default()).title
    }

    const UNBOUND_RULE_YAML: &str = "id: test-unbound\nlanguage: python\nseverity: warning\n\
        message: \"found $X call\"\nrule:\n  any:\n    - pattern: foo($X)\n    - pattern: bar()\n";

    /// A `$X` reference whose pattern branch never binds it (the `bar()` arm
    /// of the `any:` rule) must render as its literal text, not vanish.
    /// Reverting `render_message` to call `rule.get_message` unconditionally
    /// reproduces the historical defect: this assertion fails with
    /// `"found  call"` (the double space where `$X` used to be), mirroring
    /// the real-world case that motivated the fix (a Go rule's message
    /// rendering as `"defer .Close()"`).
    #[test]
    fn unbound_metavariable_left_as_literal_text() {
        let rule = parse_rule(UNBOUND_RULE_YAML);
        let title = first_match_title(&rule, "bar()\n");
        assert_eq!(
            title, "found $X call",
            "an unbound $X must render as literal text, not silently vanish"
        );
    }

    /// The same rule, matched via the branch that *does* bind `$X`, still
    /// substitutes the matched text normally — the fallback must not regress
    /// the common (bound) case.
    #[test]
    fn bound_metavariable_still_substitutes() {
        let rule = parse_rule(UNBOUND_RULE_YAML);
        let title = first_match_title(&rule, "foo(1)\n");
        assert_eq!(title, "found 1 call");
    }
}
