//! The [`Engine`] trait every backend implements, plus the normalized diagnostic
//! and format-output types backends produce.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::EngineConfig;
use crate::language::Language;

/// Which config table an [`OptionKeys`] declaration describes.
///
/// A backend that both lints and formats reads a *different* key set from each
/// of its two tables — `[lint.python.ruff] select` and `[fmt.python.ruff]
/// line_length` are not interchangeable — and the four cross-cutting backends
/// have a third, language-agnostic table on top of that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionTable {
    /// `[lint.<lang>.<engine>]`.
    Lint,
    /// `[fmt.<lang>.<engine>]`.
    Format,
    /// The language-agnostic `[lint.<engine>]` table of a cross-cutting backend
    /// (`typos`, `quality`, `uncomment`, `astgrep`). Backends without one leave
    /// this [`OptionKeys::UNCHECKED`].
    CrossCuttingLint,
}

/// The TOML value types an option key can usefully be given.
///
/// A backend reads its options with a *typed* accessor — `as_integer`,
/// `as_bool`, `as_array` — which answers `None` for a value of any other type
/// and leaves the backend on its default. Nothing about that is visible from
/// outside, so `mccabe_max_complexity = "oops"` reads exactly like a working
/// setting (issue #16). Declaring the type alongside the key is what makes the
/// difference reportable.
///
/// The variants are a bit set so a key that genuinely accepts more than one
/// shape can say so — ruff's `docstring_code_line_length` takes an integer *or*
/// the string `"dynamic"` — via [`OptionType::or`]. Widening is the safe
/// direction: an accepted type left out of a declaration is a false warning,
/// which is the one outcome worse than a missed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionType(u8);

impl OptionType {
    /// `key = true`.
    pub const BOOLEAN: OptionType = OptionType(1 << 0);
    /// `key = 10`.
    pub const INTEGER: OptionType = OptionType(1 << 1);
    /// `key = 1.5`.
    pub const FLOAT: OptionType = OptionType(1 << 2);
    /// `key = "text"`.
    pub const STRING: OptionType = OptionType(1 << 3);
    /// `key = ["a", "b"]`.
    pub const ARRAY: OptionType = OptionType(1 << 4);
    /// `key = { a = 1 }`, or the `[table.key]` sub-table form.
    pub const TABLE: OptionType = OptionType(1 << 5);
    /// `key = 1979-05-27T07:32:00Z`. No option declares it; it exists so every
    /// TOML value maps to exactly one bit and a datetime is therefore reported
    /// rather than silently unclassifiable.
    const DATETIME: OptionType = OptionType(1 << 6);

    /// A key that accepts either shape.
    #[must_use]
    pub const fn or(self, other: OptionType) -> OptionType {
        OptionType(self.0 | other.0)
    }

    /// Whether `value` is one of the shapes this declaration accepts.
    ///
    /// An integer is accepted where a float is expected — TOML spells `1` and
    /// `1.0` differently but `serde` widens the former, so rejecting it would be
    /// a false warning.
    pub fn accepts(self, value: &toml::Value) -> bool {
        let found = OptionType::of(value);
        if self.0 & found.0 != 0 {
            return true;
        }
        found == OptionType::INTEGER && self.0 & OptionType::FLOAT.0 != 0
    }

    /// The single type `value` has.
    fn of(value: &toml::Value) -> OptionType {
        match value {
            toml::Value::Boolean(_) => OptionType::BOOLEAN,
            toml::Value::Integer(_) => OptionType::INTEGER,
            toml::Value::Float(_) => OptionType::FLOAT,
            toml::Value::String(_) => OptionType::STRING,
            toml::Value::Array(_) => OptionType::ARRAY,
            toml::Value::Table(_) => OptionType::TABLE,
            toml::Value::Datetime(_) => OptionType::DATETIME,
        }
    }

    /// This type as an English noun phrase with its article, e.g. `an integer`
    /// or `an integer or a string`, for a diagnostic message.
    pub fn describe(self) -> String {
        let names = [
            (OptionType::BOOLEAN, "a boolean"),
            (OptionType::INTEGER, "an integer"),
            (OptionType::FLOAT, "a number"),
            (OptionType::STRING, "a string"),
            (OptionType::ARRAY, "an array"),
            (OptionType::TABLE, "a table"),
            (OptionType::DATETIME, "a datetime"),
        ];
        let described: Vec<&str> = names
            .iter()
            .filter(|(bit, _)| self.0 & bit.0 != 0)
            .map(|(_, name)| *name)
            .collect();
        if described.is_empty() {
            return "no value".to_string();
        }
        described.join(" or ")
    }

    /// The type of `value` as the same noun phrase [`OptionType::describe`]
    /// produces, so a message can name both sides in one vocabulary.
    pub fn describe_value(value: &toml::Value) -> String {
        OptionType::of(value).describe()
    }
}

/// The option keys a backend reads out of one of its config tables, each with
/// the value type the backend reads it as.
///
/// `[lint.*]` and `[fmt.*]` are raw `toml::Table`s with no schema, so **every**
/// key a user writes parses by construction and a key that does nothing is
/// indistinguishable from one that works. This is the declaration that makes the
/// difference visible: `poly` warns about any key in an engine's table that the
/// engine's own declaration does not cover (ADR 0016, amendment §4), and about
/// any declared key whose value the backend's own accessor cannot use (issue
/// #16).
///
/// A declaration is a claim about behaviour, so it is checked rather than
/// trusted: `engines::config_keys::tests` asserts that every declared key is one
/// the backend actually reads *at the declared type*, and that no backend leaves
/// a table [`UNCHECKED`](OptionKeys::UNCHECKED) without being on an explicit,
/// justified list.
#[derive(Debug, Clone, Copy)]
pub struct OptionKeys {
    /// `false` for [`OptionKeys::UNCHECKED`]: no key in the table is reported.
    checked: bool,
    /// Keys the backend reads by name, with the type it reads each one as.
    declared: &'static [(&'static str, OptionType)],
    /// The backend parses poly's uniform rule-selection vocabulary
    /// ([`RULE_SELECTION_KEYS`]) out of this table.
    rule_selection: bool,
    /// Derives the recognised keys from the serde type the backend deserializes
    /// the *whole* table into. `None` from the probe means "could not tell"
    /// (a malformed table), which is treated as "everything is recognised" so a
    /// type error never turns into a pile of unknown-key warnings.
    derived: Option<DerivedOptionKeys>,
    /// Extra guidance rendered as the diagnostic's description.
    note: Option<&'static str>,
}

/// A probe deriving the option keys a serde type recognises out of the table it
/// is offered. Returns `None` when the answer cannot be established. See
/// `engines::config_keys::probe`.
pub type DerivedOptionKeys = fn(&toml::Table) -> Option<Vec<String>>;

/// poly's uniform rule-selection vocabulary (ADR 0016), accepted by every
/// backend that declares [`OptionKeys::with_rule_selection`].
///
/// `rules` carries two readers: as an array it is an allow-list of rule codes,
/// as a table it is the per-rule parameter form, and backends accept both.
pub const RULE_SELECTION_KEYS: &[(&str, OptionType)] = &[
    ("select", OptionType::ARRAY),
    ("extend_select", OptionType::ARRAY),
    ("ignore", OptionType::ARRAY),
    ("rules", OptionType::ARRAY.or(OptionType::TABLE)),
];

/// Keys every per-language engine table accepts regardless of the backend.
///
/// `indent_width` is read for *any* engine by `Config::engine_config`, and the
/// `rules` sub-table's `level` overrides are applied for any engine by the
/// runner's post-lint severity remap — neither goes through the backend, so
/// neither belongs in a backend's own declaration.
///
/// Kept apart from [`ALWAYS_OPTION_KEYS`] because these two are dropped by the
/// cross-cutting merge in `Config::engine_config`, so they are accepted only in
/// a per-language table.
pub const UNIVERSAL_OPTION_KEYS: &[(&str, OptionType)] = &[
    ("indent_width", OptionType::INTEGER),
    ("rules", OptionType::ARRAY.or(OptionType::TABLE)),
];

/// Keys every engine table accepts, per-language and cross-cutting alike.
///
/// `enabled` is read by the runner's plan (`runner::plan::plan_engines`) rather
/// than by any backend, so no backend declares it and every backend honours it.
/// Three backends — `native_tool`, `uncomment`, `quality` — additionally read it
/// themselves to pick their own default, and re-declaring it there is harmless:
/// [`OptionKeys::known_keys`] dedups.
pub const ALWAYS_OPTION_KEYS: &[(&str, OptionType)] = &[(ENABLED_OPTION_KEY, OptionType::BOOLEAN)];

/// The key that switches any engine off, in any table that configures one.
pub const ENABLED_OPTION_KEY: &str = "enabled";

impl OptionKeys {
    /// The backend has not declared what it reads; nothing in the table is
    /// reported. The fallback for engines outside the registry (test stubs, the
    /// catalog tier, which is configured under `[tools.<name>]` instead).
    pub const UNCHECKED: OptionKeys = OptionKeys {
        checked: false,
        declared: &[],
        rule_selection: false,
        derived: None,
        note: None,
    };

    /// Declare the exact set of keys the backend reads from this table, each
    /// paired with the value type the backend's own accessor reads it as.
    pub const fn declared(keys: &'static [(&'static str, OptionType)]) -> OptionKeys {
        OptionKeys {
            checked: true,
            declared: keys,
            rule_selection: false,
            derived: None,
            note: None,
        }
    }

    /// Additionally accept poly's uniform rule-selection vocabulary.
    #[must_use]
    pub const fn with_rule_selection(mut self) -> OptionKeys {
        self.rule_selection = true;
        self
    }

    /// Additionally accept whatever the serde type the backend deserializes the
    /// whole table into recognises, as reported by `probe`.
    #[must_use]
    pub const fn with_derived(mut self, probe: DerivedOptionKeys) -> OptionKeys {
        self.derived = Some(probe);
        self
    }

    /// Attach guidance shown with each unknown-key warning for this table.
    #[must_use]
    pub const fn with_note(mut self, note: &'static str) -> OptionKeys {
        self.note = Some(note);
        self
    }

    /// The keys the backend declares by name for this table, with their types,
    /// excluding the uniform vocabulary and anything a derived probe recognises.
    pub fn declared_keys(&self) -> &'static [(&'static str, OptionType)] {
        self.declared
    }

    /// Whether this table's keys are checked at all.
    pub fn is_checked(&self) -> bool {
        self.checked
    }

    /// Guidance to render with an unknown-key warning for this table.
    pub fn note(&self) -> Option<&'static str> {
        self.note
    }

    /// Whether `key` is one this table accepts.
    ///
    /// `universal` adds [`UNIVERSAL_OPTION_KEYS`]; it is off for the
    /// language-agnostic cross-cutting table, whose merge in
    /// `Config::engine_config` drops both of those keys.
    pub fn accepts(&self, key: &str, options: &toml::Table, universal: bool) -> bool {
        if !self.checked {
            return true;
        }
        if self.expected_type(key, universal).is_some() {
            return true;
        }
        match self.derived {
            // `None` means the probe could not tell — accept rather than
            // invent a warning out of a parse failure.
            Some(probe) => probe(options).is_none_or(|recognized| recognized.iter().any(|k| k == key)),
            None => false,
        }
    }

    /// The type this table reads `key` as, or `None` when the answer cannot be
    /// established from the declaration alone.
    ///
    /// `None` covers both "no such key" and "the key is recognised by a derived
    /// serde probe", whose expected type is a property of an upstream type
    /// rather than anything declared here.
    pub fn expected_type(&self, key: &str, universal: bool) -> Option<OptionType> {
        if !self.checked {
            return None;
        }
        let lookup = |table: &[(&str, OptionType)]| {
            table
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, option_type)| *option_type)
        };
        // Declaration order matters only if a backend re-declares a universal
        // key (taplo's `indent_width`); the backend's own reading wins.
        lookup(self.declared)
            .or_else(|| self.rule_selection.then(|| lookup(RULE_SELECTION_KEYS)).flatten())
            .or_else(|| universal.then(|| lookup(UNIVERSAL_OPTION_KEYS)).flatten())
            .or_else(|| lookup(ALWAYS_OPTION_KEYS))
    }

    /// The type `key` should have been given, when `value` is one this table
    /// cannot use — and `None` when there is nothing to report.
    ///
    /// Silent when the expected type is unknown, and silent when a derived
    /// serde probe recognises the key as well: the upstream type is then the
    /// authority on what it accepts, not poly's hand-written declaration.
    pub fn value_problem(&self, key: &str, value: &toml::Value, universal: bool) -> Option<OptionType> {
        let expected = self.expected_type(key, universal)?;
        if expected.accepts(value) {
            return None;
        }
        if let Some(probe) = self.derived {
            let mut single = toml::Table::new();
            single.insert(key.to_string(), value.clone());
            // `None` — the probe could not tell — counts as "recognised", for
            // the same reason it does in `accepts`.
            if probe(&single).is_none_or(|recognized| recognized.iter().any(|k| k == key)) {
                return None;
            }
        }
        Some(expected)
    }

    /// Every key this table accepts, for the "recognized keys" hint on a
    /// warning. Derived keys are excluded: they are whatever a serde type
    /// accepts, which is not enumerable here.
    pub fn known_keys(&self, universal: bool) -> Vec<&'static str> {
        let mut keys: Vec<&'static str> = self.declared.iter().map(|(name, _)| *name).collect();
        if self.rule_selection {
            keys.extend(RULE_SELECTION_KEYS.iter().map(|(name, _)| *name));
        }
        if universal {
            keys.extend(UNIVERSAL_OPTION_KEYS.iter().map(|(name, _)| *name));
        }
        keys.extend(ALWAYS_OPTION_KEYS.iter().map(|(name, _)| *name));
        keys.sort_unstable();
        keys.dedup();
        keys
    }
}

/// A single file to be linted or formatted.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// Path to the file on disk.
    pub path: PathBuf,
    /// Detected language of the file.
    pub language: Language,
    /// Full file contents. Held as `Arc<str>` so a single file's bytes can be
    /// shared across every engine that runs on it (and across fix passes)
    /// without re-cloning the contents on the per-file hot path.
    pub content: Arc<str>,
}

/// What a backend is able to do for its language(s).
#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    /// The backend can report diagnostics.
    pub lint: bool,
    /// The backend can reformat source.
    pub format: bool,
    /// The backend can produce autofixes for its diagnostics.
    pub fix: bool,
}

/// Severity of a [`Diagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A problem that should fail the run.
    Error,
    /// A likely problem that does not fail the run by default.
    Warning,
    /// Informational note.
    Info,
    /// A low-priority suggestion.
    Hint,
}

/// 1-based line/column source span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Span {
    /// 1-based line of the span start.
    pub start_line: u32,
    /// 1-based column of the span start.
    pub start_col: u32,
    /// 1-based line of the span end.
    pub end_line: u32,
    /// 1-based column of the span end.
    pub end_col: u32,
}

/// A byte-range replacement used to apply an autofix.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Edit {
    /// Inclusive start byte offset into the source.
    pub start_byte: usize,
    /// Exclusive end byte offset into the source.
    pub end_byte: usize,
    /// Text to substitute for the byte range.
    pub replacement: String,
}

/// A normalized lint finding, uniform across all backends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Diagnostic {
    /// Id of the backend that produced this finding.
    pub engine: String,
    /// Tool-specific rule code, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub code: Option<String>,
    /// Severity of the finding.
    pub severity: Severity,
    /// Short, one-line title — always shown (default view).
    pub title: String,
    /// Longer explanation if the tool provides one; shown only under --verbose.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub description: Option<String>,
    /// Source location, if known.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub span: Option<Span>,
    /// Rule/doc URL if the tool exposes one; shown only under --verbose.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub url: Option<String>,
    /// Suggested autofixes, if available.  A non-empty Vec is applied
    /// atomically: either all edits apply, or none do (see `runner::apply_edits`).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub fix: Vec<Edit>,
    /// Tool-specific extras (rule URL, fix applicability, category, …), rendered
    /// verbatim by the output layer. Empty for most findings.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// Result of a format pass.
#[derive(Debug, Clone)]
pub enum FormatOutput {
    /// The input was already formatted.
    Unchanged,
    /// The formatted source (differs from the input).
    Formatted(String),
}

/// A linter/formatter backend. Backends are pure functions of
/// `(source, config)` so results can be content-hash cached.
///
/// # Stability
///
/// This trait is an **internal extension point**, not part of the stable public
/// API. Backends are implemented within this crate and reached through the
/// [`lint`](crate::lint) / [`format`](crate::format) orchestrators; implementing
/// it downstream is unsupported and may break without notice.
pub trait Engine: Send + Sync {
    /// Stable id of the **tool** this backend wraps (e.g. `"taplo"`, `"oxc"`),
    /// used as the `[<kind>.<lang>.<engine>]` config key, the `engine` field of
    /// every [`Diagnostic`] it emits, and the id component of its cache key.
    ///
    /// It names the tool, not the Rust type: a backend is named for what a user
    /// configures and reads in a report. `NixFmtEngine` is therefore
    /// `"alejandra"` — the formatter it actually wraps — and two backends that
    /// wrap two analyzers of one tool share that tool's name (`BiomeCssEngine`
    /// and `BiomeGraphqlEngine` are both `"biome"`, matching `[lint.css.biome]`
    /// and `[lint.graphql.biome]`).
    ///
    /// **Names are unique per language, not globally.** Every surface keyed by
    /// the name is resolved per language first — config tables are
    /// `[<kind>.<lang>.<engine>]`, severity remaps are compiled per engine plan,
    /// and the cache key folds in [`Engine::version`] alongside the name — so two
    /// backends may share a name only while they never serve the same language.
    /// `registry::tests::registered_engine_names_are_unique_per_language` asserts
    /// that, and `tests/engine_identity.rs` pins the rest for the `"biome"` pair.
    /// Two backends that *do* need to serve one language must take distinct names
    /// (and bump both `version()` strings so stale cache entries are invalidated).
    fn name(&self) -> &'static str;

    /// Tier-1 languages this backend explicitly handles. The generic tier may
    /// return an empty slice and rely on registry routing.
    fn languages(&self) -> &'static [Language];

    /// What this backend can do (lint/format/fix).
    fn capabilities(&self) -> Capabilities;

    /// The option keys this backend reads from `table`.
    ///
    /// The declaration `poly.toml`'s unknown-key warning checks against (ADR
    /// 0016, amendment §4). Defaults to [`OptionKeys::UNCHECKED`] so an engine
    /// outside the registry — a test stub, the catalog tier — is simply not
    /// checked; every registry backend overrides it, which
    /// `engines::config_keys::tests::every_registry_engine_declares_its_option_keys`
    /// enforces.
    ///
    /// Declaring a key the backend does not read is as much a defect as reading
    /// one it does not declare: the first invents a warning, the second hides
    /// the dead key the warning exists to expose. Both directions are asserted
    /// against the backends' own sources in `engines::config_keys::tests`.
    fn option_keys(&self, _table: OptionTable) -> OptionKeys {
        OptionKeys::UNCHECKED
    }

    /// Version of the wrapped tool/crate; folded into the cache key so a tool
    /// upgrade invalidates stale cached results.
    fn version(&self) -> &str;

    /// Whether this backend carries lint rules **for the language of the files
    /// routed to it**, under `cfg`.
    ///
    /// Consulted only on a lint plan, and only to answer one question: did
    /// anything in this run actually know how to lint this file? A `.kt` file
    /// routes to the cross-cutting backends (spell-check, ast-grep, comment
    /// removal) and to nothing that knows Kotlin, yet it was counted in
    /// `N file(s) linted` exactly like a `.py` file ruff had examined. A
    /// consumer with Kotlin, Swift and Zig in the tree read a green
    /// `poly lint .` as full coverage of languages poly has no rules for —
    /// which is the one thing this project promises never to do.
    ///
    /// The default answers from [`Engine::languages`]: a backend registered for
    /// specific languages lints those languages, and one that declares none is
    /// cross-cutting (it applies to any file, so it never establishes coverage
    /// of a *language*). Backends whose answer depends on the host or the
    /// config — a native tool that must be installed, a rule engine that needs
    /// rules — override this; a `true` here is a claim the run relies on, so it
    /// must not be made speculatively.
    ///
    /// `language` is the language being planned, since a cross-cutting backend
    /// can still hold rules for one specific language.
    fn provides_language_lint(&self, _language: &Language, _cfg: &EngineConfig) -> bool {
        !self.languages().is_empty()
    }

    /// Why this backend declines to process `src`, if it does.
    ///
    /// Some content is routed to a backend that cannot safely handle it — YAML
    /// carrying Go/Helm template actions is not valid YAML, and a Jinja template
    /// rendering Go is not markup. Formatting either corrupts it, so the backend
    /// declines.
    ///
    /// Declining is correct; doing it *invisibly* was the bug. A skipped file
    /// used to be counted as scanned and reported identically to one that was
    /// checked and found clean, so `All formatted.` could not distinguish "I
    /// verified everything" from "I declined to look". Returning the reason here
    /// — rather than bailing out inside [`Engine::lint`] / [`Engine::format`] —
    /// lets the runner count skips and the report surface them.
    ///
    /// Defaults to `None`: the backend handles everything routed to it.
    fn skip_reason(&self, _src: &SourceFile) -> Option<&'static str> {
        None
    }

    /// Lint a file, returning normalized diagnostics. Defaults to no findings.
    fn lint(&self, _src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        Ok(Vec::new())
    }

    /// Format a file. Defaults to [`FormatOutput::Unchanged`].
    fn format(&self, _src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        Ok(FormatOutput::Unchanged)
    }

    /// Whether this backend reads [`ENABLED_OPTION_KEY`] itself and degrades
    /// gracefully when it is `false`, rather than expecting the runner to drop
    /// it from the plan.
    ///
    /// The runner honours `enabled = false` by removing an engine before the
    /// file loop, which is right for a backend whose absence simply means one
    /// less check. It is wrong for a backend that *is* its language's only
    /// registry slot and hands the work to the tier-2 reindenter when switched
    /// off: removing it there leaves the language with no formatter at all, so
    /// one config key silently drops every file of that language from the run.
    ///
    /// A backend answering `true` takes on the whole contract — it must read the
    /// key in every method that acts, and must do something defensible with a
    /// `false`.
    fn self_manages_enabled(&self) -> bool {
        false
    }

    /// Whether this backend is the authoritative formatter for its language and
    /// should displace poly's generic tree-sitter reindenter.
    ///
    /// Formatters chain, so a configured external formatter running *alongside*
    /// the generic reindenter makes the two fight over indentation and prevents
    /// the format loop from converging. Only backends that are both configured
    /// and actually runnable should answer `true` — otherwise a missing binary
    /// would leave the language with no formatter at all.
    fn supersedes_generic_formatter(&self) -> bool {
        false
    }
}
