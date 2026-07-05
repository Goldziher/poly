//! Bridge between `tree-sitter-language-pack` grammars and the `ast-grep-core`
//! [`Language`](ast_grep_core::language::Language) trait.
//!
//! [`TslpLanguage`] is a thin newtype over a lowercase TSLP grammar name.  It
//! satisfies both `ast-grep-core`'s `Language` + `LanguageExt` traits by
//! delegating to TSLP's [`get_language`] — so ast-grep can parse any grammar
//! that poly's tier-2 formatter already ships, without a second grammar bundle.

use std::borrow::Cow;

use ast_grep_core::language::Language as AsgLanguage;
use ast_grep_core::matcher::{Pattern, PatternBuilder, PatternError};
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc, TSLanguage};
use serde::{Deserialize, Serialize};
use tree_sitter_language_pack::get_language;

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

// Deserialise from a YAML string like `language: python`.
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

// ── ast-grep Language trait ───────────────────────────────────────────────────

impl AsgLanguage for TslpLanguage {
    fn kind_to_id(&self, kind: &str) -> u16 {
        self.get_ts_language().id_for_node_kind(kind, /* named */ true)
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
    let mut out: Vec<char> = Vec::with_capacity(query.len());
    let mut dollar_count = 0;
    for c in query.chars() {
        if c == '$' {
            dollar_count += 1;
            continue;
        }
        // `$A`/`$$A`/`$$$A` (named, A-Z or `_`) and anonymous `$$$` get rewritten.
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

// ── ast-grep LanguageExt trait ────────────────────────────────────────────────

impl LanguageExt for TslpLanguage {
    fn get_ts_language(&self) -> TSLanguage {
        // Invariant: `name` was validated at construction time — both
        // `TslpLanguage::new` and `Deserialize` call `get_language` to confirm
        // the grammar exists — so this lookup cannot fail. (No `unsafe` here.)
        get_language(&self.name).expect("TslpLanguage grammar was validated at construction; get_language must succeed")
    }
}
