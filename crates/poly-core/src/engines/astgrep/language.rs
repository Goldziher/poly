//! Bridge between `tree-sitter-language-pack` grammars and the `ast-grep-core`
//! [`Language`](ast_grep_core::language::Language) trait.
//!
//! [`TslpLanguage`] is a thin newtype over a lowercase TSLP grammar name.  It
//! satisfies both `ast-grep-core`'s `Language` + `LanguageExt` traits by
//! delegating to TSLP's [`get_language`] — so ast-grep can parse any grammar
//! that poly's tier-2 formatter already ships, without a second grammar bundle.
//!
//! [`grammar_for_language_id`] is the other half of the bridge: poly's own
//! language vocabulary is not TSLP's, so a file's [`Language::id`] has to be
//! translated to the grammar that parses it before any rule can be looked up.
//!
//! [`Language::id`]: crate::language::Language::id

use std::borrow::Cow;

use ast_grep_core::language::Language as AsgLanguage;
use ast_grep_core::matcher::{Pattern, PatternBuilder, PatternError};
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc, TSLanguage};
use serde::{Deserialize, Serialize};
use tree_sitter_language_pack::get_language;

/// poly language ids that are **not** `tree-sitter-language-pack` grammar
/// names, paired with the grammar that parses that language.
///
/// [`Language::id`](crate::language::Language::id) is poly's own vocabulary: it
/// keys `[lint.<lang>.<tool>]` config tables, the `engine`/language fields of a
/// report, and the cache key, so it cannot be redefined to whatever TSLP happens
/// to call a grammar. Five ids differ from any grammar name, and ast-grep needs
/// the grammar — for the rule-map lookup as much as for parsing, since a rule's
/// `language:` deserializes to a *validated* [`TslpLanguage`] and is therefore
/// always stored under a grammar name.
///
/// Without the mapping those five languages have no ast-grep coverage that any
/// configuration can restore: no rule can be keyed to `jsx` (`language: jsx`
/// fails to deserialize), and a rule keyed `javascript` is never looked up for a
/// `.jsx` file.
///
/// Each entry, and why that grammar:
///
/// * `jsx` → `javascript` — TSLP's own manifest lists `jsx` as an extension of
///   the `javascript` grammar, which parses JSX elements natively.
/// * `jsonc` → `json` — `tree-sitter-json` carries `comment` in its `extras`,
///   so JSON-with-comments parses without error.
/// * `mdx` → `markdown` — MDX is Markdown plus ESM/JSX; the `markdown` grammar
///   parses the Markdown structure and treats the JSX as an HTML block, so
///   Markdown rules apply and JSX-shaped rules simply never match.
/// * `jinja` → `jinja2` — the same language under TSLP's longer spelling.
/// * `mustache` → `glimmer` — `glimmer` is TSLP's Handlebars grammar (its
///   declared extension is `hbs`), and Handlebars is a superset of Mustache.
///
/// A poly id that *is* a grammar name (or a TSLP alias, such as `shell` →
/// `bash`) must not be listed here; `every_language_id_resolves_to_a_grammar`
/// is the guard that the list stays complete.
const GRAMMAR_FOR_LANGUAGE_ID: &[(&str, &str)] = &[
    ("jsx", "javascript"),
    ("jsonc", "json"),
    ("mdx", "markdown"),
    ("jinja", "jinja2"),
    ("mustache", "glimmer"),
];

/// The TSLP grammar name that parses poly's `language_id`.
///
/// The identity for every id TSLP already knows; see
/// [`GRAMMAR_FOR_LANGUAGE_ID`] for the five that differ and why.
pub fn grammar_for_language_id(language_id: &str) -> &str {
    GRAMMAR_FOR_LANGUAGE_ID
        .iter()
        .find(|(id, _)| *id == language_id)
        .map_or(language_id, |(_, grammar)| *grammar)
}

/// A TSLP-backed language value for ast-grep.
///
/// Wraps a lowercase grammar name (e.g. `"python"`, `"go"`) and satisfies:
/// - `ast_grep_core::language::Language` — needed by every ast-grep generic.
/// - `ast_grep_core::tree_sitter::LanguageExt` — provides the raw
///   `tree_sitter::Language` that backs parsing and pattern compilation.
/// - `serde::Deserialize` — lets `from_yaml_string` deserialize the `language:`
///   field in user rule YAML files.
///
/// Construction validates that the name is known to TSLP; an unknown name
/// produces a `Deserialize` / `TryFrom` error before any rule is compiled.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct TslpLanguage {
    /// Lowercase TSLP grammar name, e.g. `"python"`.
    pub(crate) name: String,
}

impl TslpLanguage {
    /// Construct a `TslpLanguage` from a grammar name, returning `None` if TSLP
    /// does not recognise the name.
    pub fn new(name: &str) -> Option<Self> {
        let lowered = name.to_lowercase();
        get_language(&lowered).ok().map(|_| TslpLanguage { name: lowered })
    }

    /// Grammar name (lowercase, as TSLP expects it).
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl<'de> Deserialize<'de> for TslpLanguage {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(de)?;
        TslpLanguage::new(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown language '{}': not found in tree-sitter-language-pack",
                raw
            ))
        })
    }
}

impl AsgLanguage for TslpLanguage {
    fn kind_to_id(&self, kind: &str) -> u16 {
        self.get_ts_language().id_for_node_kind(kind, true)
    }

    fn field_to_id(&self, field: &str) -> Option<u16> {
        self.get_ts_language().field_id_for_name(field).map(|f| f.get())
    }

    /// The identifier-legal character `$` is rewritten to before parsing.
    ///
    /// Most grammars reject `$` in identifiers, so `$META` cannot be parsed by
    /// tree-sitter as-is. ast-grep swaps `$` for an "expando" char that IS a
    /// valid identifier, parses, then maps back. The per-grammar choice mirrors
    /// `ast-grep-language` exactly; grammars that accept `$` use `$` (a no-op).
    fn expando_char(&self) -> char {
        expando_char_for(&self.name)
    }

    fn pre_process_pattern<'q>(&self, query: &'q str) -> Cow<'q, str> {
        let expando = self.expando_char();
        if expando == '$' {
            return Cow::Borrowed(query);
        }
        rewrite_sigils(expando, query)
    }

    fn build_pattern(&self, builder: &PatternBuilder) -> Result<Pattern, PatternError> {
        builder.build(|src| StrDoc::try_new(src, self.clone()))
    }
}

/// Per-grammar expando character, mirroring `ast-grep-language`'s choices.
///
/// Returns `$` for grammars that accept `$` as an identifier char (no rewrite),
/// `_` for the CSS family and Nix, `𐀀` (U+10000) for C/C++, and `µ` — the
/// ast-grep default — for everything else.
fn expando_char_for(name: &str) -> char {
    match name {
        "bash" | "shell" | "java" | "javascript" | "jsx" | "json" | "jsonc" | "lua" | "markdown" | "scala"
        | "solidity" | "tsx" | "typescript" | "dart" | "yaml" => '$',
        "css" | "scss" | "less" | "nix" => '_',
        "c" | "cpp" | "c++" => '\u{10000}',
        _ => 'µ',
    }
}

/// Rewrite ast-grep metavariable sigils (`$NAME`, `$$NAME`, `$$$`) from `$` to
/// `expando` so the target grammar can parse them as identifiers. Ported from
/// `ast_grep_language::pre_process_pattern`.
fn rewrite_sigils(expando: char, query: &str) -> Cow<'_, str> {
    if !query.contains('$') {
        return Cow::Borrowed(query);
    }
    let mut out: Vec<char> = Vec::with_capacity(query.len());
    let mut dollar_count = 0;
    for c in query.chars() {
        if c == '$' {
            dollar_count += 1;
            continue;
        }
        let need_replace = matches!(c, 'A'..='Z' | '_') || dollar_count == 3;
        let sigil = if need_replace { expando } else { '$' };
        out.extend(std::iter::repeat_n(sigil, dollar_count));
        dollar_count = 0;
        out.push(c);
    }
    let sigil = if dollar_count == 3 { expando } else { '$' };
    out.extend(std::iter::repeat_n(sigil, dollar_count));
    Cow::Owned(out.into_iter().collect())
}

impl LanguageExt for TslpLanguage {
    fn get_ts_language(&self) -> TSLanguage {
        get_language(&self.name).expect("TslpLanguage grammar was validated at construction; get_language must succeed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::all_languages;

    /// The guard on [`GRAMMAR_FOR_LANGUAGE_ID`]: every language poly can detect
    /// must reach a grammar ast-grep can parse with, or that language silently
    /// has no custom-rule coverage at all. `all_languages` is itself held
    /// exhaustive by a compile-time match in `registry`, so a newly added
    /// `Language` variant reaches this assertion.
    #[test]
    fn every_language_id_resolves_to_a_grammar() {
        for language in all_languages() {
            let grammar = grammar_for_language_id(language.id());
            assert!(
                TslpLanguage::new(grammar).is_some(),
                "{} maps to grammar {grammar:?}, which tree-sitter-language-pack does not ship",
                language.id()
            );
        }
    }

    /// The inverse guard: an entry whose id TSLP *does* know would silently
    /// redirect a language away from its own grammar.
    #[test]
    fn no_mapping_shadows_a_real_grammar() {
        for (id, grammar) in GRAMMAR_FOR_LANGUAGE_ID {
            assert!(
                TslpLanguage::new(id).is_none(),
                "{id:?} is a grammar name in its own right; mapping it to {grammar:?} hides it"
            );
        }
    }

    /// The mapping is applied to the *file's* language, so it must be the
    /// identity for every id that already names a grammar.
    #[test]
    fn a_known_grammar_name_maps_to_itself() {
        assert_eq!(grammar_for_language_id("python"), "python");
        assert_eq!(grammar_for_language_id("typescript"), "typescript");
        assert_eq!(grammar_for_language_id("tsx"), "tsx");
        assert_eq!(grammar_for_language_id("shell"), "shell", "TSLP aliases `shell` itself");
    }

    #[test]
    fn jsx_maps_to_the_javascript_grammar() {
        assert_eq!(grammar_for_language_id("jsx"), "javascript");
    }
}
