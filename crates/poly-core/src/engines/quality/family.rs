//! The per-language deferral table (ADR 0027, decision 3): the `quality` engine
//! never re-implements a metric a tier-1 backend already selects by default.
//!
//! Every language routes to one of four families, matched against the tier-1
//! backend that already owns the metric:
//!
//! - [`Family::Python`] — ruff.
//! - [`Family::JsTs`] — oxlint (JavaScript/TypeScript/JSX/TSX only; Vue, Svelte
//!   and Astro get no oxlint pass today and fall into [`Family::Other`]).
//! - [`Family::Php`] — mago.
//! - [`Family::Other`] — every language with no tier-1 lint backend, plus the
//!   template/markup languages oxlint does not reach.
//!
//! # The table (traced to source at the pinned revs; see
//! `~/.claude/plans/quality-tier-3a-reference.md` §1 for the full derivation)
//!
//! | Rule | Deferred for | Reason |
//! |---|---|---|
//! | `too-many-parameters` | Python, PHP | ruff `PLR0913`+`PLR0917` (default 5, stricter than poly's 6); mago `excessive-parameter-list` (default 5) |
//! | `nesting-too-deep` | JS/TS, PHP | oxlint `max-depth` (`pedantic`, on, default 4 — identical definition); mago `excessive-nesting` (default 7) |
//! | `cyclomatic-complexity` | Python, PHP | ruff `C901` (`DEFAULT_MAX_COMPLEXITY` = 10, stricter than poly's 20); mago `cyclomatic-complexity` (default 15) |
//!
//! `file-too-long`, `function-too-long`, `type-too-long`, `lazy-ignore`,
//! `magic-number` and `law-of-demeter` are never deferred: no tier-1 backend
//! in this repo selects an equivalent rule for any language.
//!
//! **Trap already found and pinned by a test below:** ruff's `PLR1702`
//! (too-many-nested-blocks) sits in `RULE_CODES` but is preview-gated
//! (`Rule::is_preview`) and `PreviewMode` defaults to `Disabled`
//! (`engines/ruff.rs:162`), so it never actually runs — Python is **not**
//! deferred for `nesting-too-deep`. Asserting this against `Rule::is_preview`
//! rather than against the rule selector is deliberate: the day ruff
//! stabilizes `PLR1702`, this table must flip, and a selector-only assertion
//! would stay green while poly silently doubled every Python nesting finding.

use crate::language::Language;

/// The tier-1 lint family a language belongs to, for deferral purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Routed to ruff.
    Python,
    /// Routed to oxlint: JavaScript, TypeScript, JSX, TSX only.
    JsTs,
    /// Routed to mago.
    Php,
    /// Everything else: no tier-1 lint backend, or a markup/template
    /// language oxlint does not reach (Vue, Svelte, Astro, Less, HTML, XML,
    /// …), including the whole [`Language::Other`] tail.
    Other,
}

/// One rule this engine can emit, for deferral-table lookups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// `file-too-long`.
    FileTooLong,
    /// `function-too-long`.
    FunctionTooLong,
    /// `type-too-long`.
    TypeTooLong,
    /// `too-many-parameters`.
    TooManyParameters,
    /// `nesting-too-deep`.
    NestingTooDeep,
    /// `cyclomatic-complexity`.
    CyclomaticComplexity,
    /// `lazy-ignore`.
    LazyIgnore,
    /// `magic-number` (opt-in).
    MagicNumber,
    /// `law-of-demeter` (opt-in).
    LawOfDemeter,
}

impl Rule {
    /// The stable diagnostic code emitted for this rule.
    pub fn code(self) -> &'static str {
        match self {
            Rule::FileTooLong => "file-too-long",
            Rule::FunctionTooLong => "function-too-long",
            Rule::TypeTooLong => "type-too-long",
            Rule::TooManyParameters => "too-many-parameters",
            Rule::NestingTooDeep => "nesting-too-deep",
            Rule::CyclomaticComplexity => "cyclomatic-complexity",
            Rule::LazyIgnore => "lazy-ignore",
            Rule::MagicNumber => "magic-number",
            Rule::LawOfDemeter => "law-of-demeter",
        }
    }
}

impl Family {
    /// Classify a [`Language`] into its tier-1 deferral family.
    pub fn of(language: &Language) -> Family {
        match language {
            Language::Python => Family::Python,
            Language::JavaScript | Language::TypeScript | Language::Jsx | Language::Tsx => Family::JsTs,
            Language::Php => Family::Php,
            _ => Family::Other,
        }
    }
}

/// Whether `rule` is deferred to an existing tier-1 backend for `language`.
///
/// A deferred rule emits **nothing** for that language: the tier-1 backend
/// already reports the equivalent finding, and reporting it twice — in two
/// vocabularies, from two engines — is strictly worse than the coverage gap
/// this engine exists to close. See the module docs for the full table and
/// the citations behind each row.
pub fn is_deferred(rule: Rule, language: &Language) -> bool {
    let family = Family::of(language);
    matches!(
        (rule, family),
        (Rule::TooManyParameters, Family::Python | Family::Php)
            | (Rule::NestingTooDeep, Family::JsTs | Family::Php)
            | (Rule::CyclomaticComplexity, Family::Python | Family::Php)
    )
}

#[cfg(test)]
mod tests {
    use super::{Family, Rule, is_deferred};
    use crate::language::Language;

    /// Pins the deferral table exactly as derived in the reference doc. If a
    /// tier-1 backend's default rule set changes, this test — not a silent
    /// double-report in production — is where that surfaces.
    #[test]
    fn deferral_table_matches_reference() {
        let cases: &[(Rule, Language, bool)] = &[
            (Rule::TooManyParameters, Language::Python, true),
            (Rule::TooManyParameters, Language::Php, true),
            (Rule::TooManyParameters, Language::TypeScript, false),
            (Rule::TooManyParameters, Language::Go, false),
            (Rule::NestingTooDeep, Language::TypeScript, true),
            (Rule::NestingTooDeep, Language::JavaScript, true),
            (Rule::NestingTooDeep, Language::Jsx, true),
            (Rule::NestingTooDeep, Language::Tsx, true),
            (Rule::NestingTooDeep, Language::Php, true),
            (Rule::NestingTooDeep, Language::Python, false),
            (Rule::NestingTooDeep, Language::Go, false),
            (Rule::CyclomaticComplexity, Language::Python, true),
            (Rule::CyclomaticComplexity, Language::Php, true),
            (Rule::CyclomaticComplexity, Language::TypeScript, false),
            (Rule::CyclomaticComplexity, Language::Go, false),
            // Never deferred, in any family.
            (Rule::FileTooLong, Language::Python, false),
            (Rule::FunctionTooLong, Language::Php, false),
            (Rule::TypeTooLong, Language::TypeScript, false),
            (Rule::LazyIgnore, Language::Python, false),
            (Rule::MagicNumber, Language::Php, false),
            (Rule::LawOfDemeter, Language::Go, false),
        ];
        for (rule, language, expected) in cases {
            assert_eq!(
                is_deferred(*rule, language),
                *expected,
                "rule {:?} for {language:?} expected deferred={expected}",
                rule.code(),
            );
        }
    }

    /// The trap named in the module docs: `PLR1702` looks like it would defer
    /// Python's `nesting-too-deep`, but it is preview-gated and therefore
    /// never runs, so Python must still get the rule. Asserted against
    /// `Rule::is_preview` (not the rule selector) so a future stabilization
    /// flips this test rather than poly silently double-reporting.
    #[test]
    fn python_nesting_too_deep_is_not_deferred_because_plr1702_is_preview_only() {
        use ruff_linter::registry::Rule as RuffRule;

        assert!(
            RuffRule::TooManyNestedBlocks.is_preview(),
            "PLR1702 (too-many-nested-blocks) must stay preview-gated; if ruff has \
             stabilized it, Python's nesting-too-deep must move to the deferred set \
             in `is_deferred` above and this test must be updated to reflect that",
        );
        assert!(
            !is_deferred(Rule::NestingTooDeep, &Language::Python),
            "PLR1702 is preview-only (never runs), so poly's own nesting-too-deep \
             must still run for Python",
        );
    }

    #[test]
    fn family_classification() {
        assert_eq!(Family::of(&Language::Python), Family::Python);
        assert_eq!(Family::of(&Language::TypeScript), Family::JsTs);
        assert_eq!(Family::of(&Language::Jsx), Family::JsTs);
        assert_eq!(Family::of(&Language::Tsx), Family::JsTs);
        assert_eq!(Family::of(&Language::JavaScript), Family::JsTs);
        assert_eq!(Family::of(&Language::Php), Family::Php);
        assert_eq!(Family::of(&Language::Go), Family::Other);
        assert_eq!(Family::of(&Language::Vue), Family::Other);
        assert_eq!(Family::of(&Language::Other("elm".to_owned())), Family::Other);
    }
}
