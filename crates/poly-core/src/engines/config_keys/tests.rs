//! Tests for the unknown-option-key check.
//!
//! Three groups, in order of what they protect:
//!
//! 1. **Behaviour** — a misspelled key warns exactly once, naming the key and
//!    the backend; a correctly spelled one does not; a correctly spelled one
//!    given an unusable value warns under its own separate code.
//! 2. **Honesty of the declarations** — every registry backend declares its
//!    keys, every declared key is one the backend's own source reads *at the
//!    declared type*, and every option read in a backend's source is declared.
//!    Without these the check would just move the original defect (a list that
//!    silently drifts from the code) into a new file.
//! 3. **Liveness** — setting a declared key changes what the backend produces.
//!    "The key is listed" and "the key works" are different claims.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use regex::Regex;

use super::*;
use crate::config::{Config, EngineConfig, GlobalDefaults, Kind};
use crate::engine::{Engine, FormatOutput, OptionTable, OptionType, Severity};
use crate::registry::{all_languages, engines_for};

/// Lint a `poly.toml` source through the backend, exactly as the runner would.
fn warnings(source: &str) -> Vec<Diagnostic> {
    lint_config("poly.toml", source)
}

/// Lint `source` as if it were the file `name`.
fn lint_config(name: &str, source: &str) -> Vec<Diagnostic> {
    let src = SourceFile {
        path: PathBuf::from(name),
        language: Language::Toml,
        content: source.into(),
    };
    let cfg = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    };
    PolyConfigEngine.lint(&src, &cfg).expect("lint")
}

fn titles(diagnostics: &[Diagnostic]) -> Vec<String> {
    diagnostics.iter().map(|d| d.title.clone()).collect()
}

// ---------------------------------------------------------------- behaviour

#[test]
fn a_misspelled_key_is_reported_once_naming_the_key_and_the_engine() {
    let found = warnings("[lint.python.ruff]\nmccabe_max_complexit = 10\n");
    assert_eq!(found.len(), 1, "expected exactly one warning, got {:?}", titles(&found));
    assert_eq!(
        found[0].title,
        "unknown option `mccabe_max_complexit` in `[lint.python.ruff]`: the ruff backend does not read it"
    );
    assert_eq!(found[0].severity, Severity::Warning);
    assert_eq!(found[0].code.as_deref(), Some("unknown-config-key"));
}

#[test]
fn a_correctly_spelled_key_is_not_reported() {
    let found = warnings("[lint.python.ruff]\nmccabe_max_complexity = 10\n");
    assert!(found.is_empty(), "unexpected warnings: {:?}", titles(&found));
}

#[test]
fn an_unknown_key_never_fails_the_run() {
    // Warning severity only: `poly lint` exits non-zero on errors, and a config
    // typo must not break a run that was passing yesterday.
    let found = warnings("[fmt.toml.taplo]\nreorder_key = true\n");
    assert!(found.iter().all(|d| d.severity == Severity::Warning));
}

#[test]
fn the_warning_points_at_the_offending_key() {
    let found = warnings("[defaults]\nline_length = 120\n\n[fmt.toml.taplo]\nreorder_key = true\n");
    let span = found[0].span.expect("span");
    assert_eq!((span.start_line, span.start_col), (5, 1));
}

#[test]
fn the_warning_lists_the_keys_the_table_does_accept() {
    let found = warnings("[fmt.python.ruff]\nnope = 1\n");
    let description = found[0].description.clone().expect("description");
    assert!(
        description.contains("docstring_code_format"),
        "description should name the real keys: {description}"
    );
}

#[test]
fn each_engine_table_is_reported_separately() {
    let found = warnings("[lint.python.ruff]\nbogus_a = 1\n\n[fmt.python.ruff]\nbogus_b = 1\n");
    assert_eq!(found.len(), 2, "{:?}", titles(&found));
    assert!(found[0].title.contains("[lint.python.ruff]"));
    assert!(found[1].title.contains("[fmt.python.ruff]"));
}

#[test]
fn a_lint_only_key_written_in_the_format_table_is_reported() {
    // The exact shape of the audited defect: four documented `[fmt.python.ruff]`
    // keys were dead, and nothing said so.
    let found = warnings("[fmt.python.ruff]\nselect = [\"E\"]\n");
    assert_eq!(found.len(), 1, "{:?}", titles(&found));
    assert!(found[0].title.contains("`select`"));
}

#[test]
fn per_rule_parameters_are_not_reported() {
    // ADR 0016: `[rules.<id>]` holds `level` plus arbitrary tool parameters.
    let found =
        warnings("[lint.javascript.oxc.rules.max-params]\nlevel = \"warning\"\nmax = 6\nanything_at_all = true\n");
    assert!(found.is_empty(), "unexpected warnings: {:?}", titles(&found));
}

#[test]
fn the_uniform_vocabulary_is_accepted_only_where_the_backend_parses_it() {
    let accepted = warnings("[lint.sql.sqruff]\nselect = [\"L001\"]\nextend_select = []\nignore = []\n");
    assert!(accepted.is_empty(), "unexpected warnings: {:?}", titles(&accepted));

    // typos has no rule selection: its merge drops those keys entirely.
    let rejected = warnings("[lint.python.typos]\nselect = [\"E\"]\n");
    assert_eq!(rejected.len(), 1, "{:?}", titles(&rejected));
}

#[test]
fn the_universal_keys_are_accepted_in_any_per_language_table() {
    let found = warnings(
        "[fmt.css.malva]\nindent_width = 4\n\n[lint.css.biome.rules.noUnknownProperty]\nlevel = \"warning\"\n",
    );
    assert!(found.is_empty(), "unexpected warnings: {:?}", titles(&found));
}

/// `enabled` is read by the runner's plan rather than by any backend, so no
/// backend declares it — and every table that configures an engine must still
/// accept it. Before it was universal, `[lint.python.ruff] enabled = false`
/// was reported as an unknown key *and* ruff ran anyway.
#[test]
fn enabled_is_accepted_in_every_engine_table() {
    let found = warnings(
        "[lint.python.ruff]\nenabled = false\n\n[lint.typos]\nenabled = false\n\n[lint.python.astgrep]\nenabled = false\n",
    );
    assert!(found.is_empty(), "unexpected warnings: {:?}", titles(&found));
}

/// It is a boolean everywhere, so a non-boolean is still reported — an
/// always-accepted key must not become an unchecked one.
#[test]
fn enabled_given_a_non_boolean_is_reported() {
    let found = warnings("[lint.python.ruff]\nenabled = \"yes\"\n");
    assert_eq!(found.len(), 1, "{:?}", titles(&found));
}

#[test]
fn an_unknown_key_in_a_cross_cutting_table_is_reported() {
    let found = warnings("[lint.typos]\nextend_word = { teh = \"the\" }\n");
    assert_eq!(found.len(), 1, "{:?}", titles(&found));
    assert!(found[0].title.contains("the typos backend"));
}

#[test]
fn a_per_language_astgrep_table_is_reported_because_it_is_never_read() {
    // `Config::build_astgrep_options` consults only `[lint.astgrep]`, so every
    // key under a per-language table there is dead config.
    let found = warnings("[lint.astgrep]\nselect = [\"no-console\"]\n\n[lint.python.astgrep]\nselect = [\"x\"]\n");
    assert_eq!(found.len(), 1, "{:?}", titles(&found));
    assert!(found[0].title.contains("[lint.python.astgrep]"));
    assert!(
        found[0]
            .description
            .as_deref()
            .is_some_and(|note| note.contains("language-agnostic")),
        "{:?}",
        found[0].description
    );
}

#[test]
fn an_unknown_tool_or_language_name_is_left_alone() {
    // A catalog tool (ADR 0013) owns a table named after itself and may be
    // declared in a base config this file cannot see; `Language::Other` means
    // any tree-sitter id is a real language. Neither can be judged from here.
    let found = warnings("[lint.python.notatool]\nwhatever = 1\n\n[fmt.notalanguage.notatool]\nwhatever = 1\n");
    assert!(found.is_empty(), "unexpected warnings: {:?}", titles(&found));
}

#[test]
fn a_toml_file_that_is_not_a_poly_config_is_not_checked() {
    let found = lint_config("Cargo.toml", "[lint.python.ruff]\nbogus = 1\n");
    assert!(found.is_empty(), "unexpected warnings: {:?}", titles(&found));
}

#[test]
fn a_local_override_file_is_checked() {
    let found = lint_config("poly.local.toml", "[lint.python.ruff]\nbogus = 1\n");
    assert_eq!(found.len(), 1, "{:?}", titles(&found));
}

#[test]
fn an_unparsable_config_is_left_to_the_config_loader() {
    let found = warnings("[lint.python.ruff\nbogus = 1\n");
    assert!(found.is_empty(), "unexpected warnings: {:?}", titles(&found));
}

#[test]
fn keys_the_upstream_formatter_type_recognizes_are_accepted() {
    // malva's `FormatOptions` flattens its layout/language sub-structs, so the
    // recognised set is derived from the type rather than copied from it.
    let found = warnings(
        "[fmt.css.malva]\nprint_width = 100\nprintWidth = 100\nhex_case = \"lower\"\nsingle_line_top_level_declarations = true\n",
    );
    assert!(found.is_empty(), "unexpected warnings: {:?}", titles(&found));

    let misspelled = warnings("[fmt.css.malva]\nhex_cased = \"lower\"\n");
    assert_eq!(misspelled.len(), 1, "{:?}", titles(&misspelled));
}

#[test]
fn an_upstream_dotted_option_is_accepted_in_the_one_form_that_works() {
    // `attr_selector.quotes` is a single serde field name upstream, so only
    // TOML's quoted-key form reaches it...
    let quoted = warnings("[fmt.css.malva]\n\"attr_selector.quotes\" = \"always double\"\n");
    assert!(quoted.is_empty(), "unexpected warnings: {:?}", titles(&quoted));

    // ...while the sub-table form produces a key named `attr_selector`, which
    // malva ignores. That is dead config, and saying so is the whole point.
    let sub_table = warnings("[fmt.css.malva.attr_selector]\nquotes = \"always double\"\n");
    assert_eq!(sub_table.len(), 1, "{:?}", titles(&sub_table));
}

#[test]
fn an_optional_upstream_field_is_not_mistaken_for_an_unknown_key() {
    // These four default to `None` and vanish from a serialized default, which
    // is why the recognised set is probed by type rather than by serialization.
    let found = warnings(
        "[fmt.css.malva]\nhex_color_length = \"short\"\ndeclaration_order = \"alphabetical\"\nsingle_line_block_threshold = 1\nkeyframe_selector_notation = \"percentage\"\n",
    );
    assert!(found.is_empty(), "unexpected warnings: {:?}", titles(&found));
}

#[test]
fn the_mago_format_settings_are_derived_from_the_upstream_type() {
    // mago's `RawFormatSettings` is kebab-case and ~96 fields wide; a copied
    // list would drift on the next upgrade.
    let found = warnings("[fmt.php.mago]\nprint-width = 100\nsort-uses = true\n");
    assert!(found.is_empty(), "unexpected warnings: {:?}", titles(&found));

    let misspelled = warnings("[fmt.php.mago]\nsort-use = true\n");
    assert_eq!(misspelled.len(), 1, "{:?}", titles(&misspelled));
}

#[test]
fn a_native_toolchain_table_accepts_only_enabled() {
    assert!(warnings("[fmt.go.gofmt]\nenabled = false\n").is_empty());
    let found = warnings("[fmt.go.gofmt]\nargs = [\"-s\"]\n");
    assert_eq!(found.len(), 1, "{:?}", titles(&found));
}

#[test]
fn a_declared_key_given_an_unusable_value_is_reported_under_its_own_code() {
    // Issue #16: `as_integer` answers `None` and ruff keeps its default, so the
    // config looks enforced and is not.
    let found = warnings("[lint.python.ruff]\nmccabe_max_complexity = \"oops\"\n");
    assert_eq!(found.len(), 1, "{:?}", titles(&found));
    assert_eq!(found[0].code.as_deref(), Some("invalid-config-value"));
    assert_eq!(
        found[0].title,
        "invalid value for `mccabe_max_complexity` in `[lint.python.ruff]`: expected an integer, found a string"
    );
    assert_eq!(found[0].severity, Severity::Warning);
    let description = found[0].description.as_deref().expect("description");
    assert!(
        description.contains("discarded") && description.contains("no effect"),
        "the description must state the consequence: {description}"
    );
}

#[test]
fn a_key_that_accepts_two_types_is_reported_for_neither() {
    // ruff's `docstring_code_line_length` takes a width *or* `"dynamic"`.
    let integer = warnings("[fmt.python.ruff]\ndocstring_code_line_length = 100\n");
    assert!(integer.is_empty(), "{:?}", titles(&integer));
    let dynamic = warnings("[fmt.python.ruff]\ndocstring_code_line_length = \"dynamic\"\n");
    assert!(dynamic.is_empty(), "{:?}", titles(&dynamic));
    let neither = warnings("[fmt.python.ruff]\ndocstring_code_line_length = true\n");
    assert_eq!(neither.len(), 1, "{:?}", titles(&neither));
    assert!(
        neither[0].title.contains("an integer or a string"),
        "both accepted types belong in the message: {}",
        neither[0].title
    );
}

#[test]
fn the_uniform_vocabulary_is_type_checked_too() {
    // `select` is read with `as_array`; a bare string is silently dropped.
    let found = warnings("[lint.sql.sqruff]\nselect = \"L001\"\n");
    assert_eq!(found.len(), 1, "{:?}", titles(&found));
    assert_eq!(found[0].code.as_deref(), Some("invalid-config-value"));
}

#[test]
fn the_rules_key_is_accepted_in_both_of_its_forms() {
    // ADR 0016: an array of codes *or* a table of per-rule parameters.
    let array = warnings("[lint.sql.sqruff]\nrules = [\"LT05\"]\n");
    assert!(array.is_empty(), "{:?}", titles(&array));
    let table = warnings("[lint.sql.sqruff.rules.LT05]\nlevel = \"warning\"\n");
    assert!(table.is_empty(), "{:?}", titles(&table));
}

#[test]
fn a_universal_key_given_an_unusable_value_is_reported() {
    // `indent_width` is read by `Config::engine_config` for every engine, and
    // read as an integer — so it is type-checked in a table whose backend does
    // not itself parse it. (Not malva's: its upstream `FormatOptions` has an
    // `indent_width` of its own and, per the rule above, that type decides.)
    let found = warnings("[lint.python.ruff]\nindent_width = \"four\"\n");
    assert_eq!(found.len(), 1, "{:?}", titles(&found));
    assert_eq!(found[0].code.as_deref(), Some("invalid-config-value"));
}

#[test]
fn a_cross_cutting_key_given_an_unusable_value_is_reported() {
    let found = warnings("[lint.typos]\nextend_words = [\"teh\"]\n");
    assert_eq!(found.len(), 1, "{:?}", titles(&found));
    assert!(
        found[0].title.contains("expected a table, found an array"),
        "{}",
        found[0].title
    );
}

#[test]
fn a_key_only_an_upstream_serde_type_recognizes_is_not_type_checked() {
    // The false-positive boundary. malva's own `FormatOptions` decides what
    // `print_width` accepts; poly declares no type for it and so cannot name
    // one, and a guess would be exactly the untrustworthy warning this module
    // refuses to emit.
    let found = warnings("[fmt.css.malva]\nprint_width = \"wide\"\n");
    assert!(found.is_empty(), "unexpected warnings: {:?}", titles(&found));
}

#[test]
fn a_declared_key_an_upstream_type_also_recognizes_defers_to_the_upstream_type() {
    // markup_fmt declares the oxc formatter keys (Astro `<script>` blocks are
    // formatted from *its* table) while also deriving keys from its own
    // `FormatOptions`. Where both could answer, the upstream type wins: it is
    // the one that actually parses the table.
    let engine = registry_engines()
        .into_iter()
        .find(|engine| engine.name() == "markup_fmt")
        .expect("markup_fmt is registered");
    let keys = engine.option_keys(OptionTable::Format);
    for (key, _) in keys.declared_keys() {
        let mut table = toml::Table::new();
        // A datetime is the one value no option in either schema accepts, so a
        // `None` here means the declaration alone decided — which is what this
        // test wants to observe rather than assert away.
        table.insert((*key).to_string(), toml::Value::Boolean(true));
        let reported = keys.value_problem(key, &table[*key], true).is_some();
        let recognized_upstream = keys.accepts(key, &table, true);
        assert!(
            recognized_upstream,
            "`{key}` must stay an accepted key of `[fmt.html.markup_fmt]`"
        );
        assert_eq!(
            reported,
            keys.expected_type(key, true) == Some(OptionType::STRING),
            "`{key}`: a boolean is reported exactly when the declaration says the key is a string"
        );
    }
}

// ------------------------------------------------- honesty of declarations

/// Where each backend's option reads live, so the two drift tests below can
/// compare a declaration against the code it claims to describe.
///
/// Exhaustiveness is enforced by
/// [`every_registry_engine_has_declared_sources`]: a backend added to the
/// registry without an entry here fails that test rather than silently escaping
/// both drift checks.
const ENGINE_SOURCES: &[(&str, &[&str])] = &[
    ("ruff", &["ruff.rs"]),
    ("oxc", &["oxc/mod.rs", "oxc/config.rs", "oxc/lint.rs", "oxc/format.rs"]),
    (
        "mago",
        &["mago/mod.rs", "mago/lint.rs", "mago/format.rs", "mago/rules.rs"],
    ),
    ("sqruff", &["sqruff.rs"]),
    ("rumdl", &["rumdl.rs"]),
    ("taplo", &["taplo.rs"]),
    ("yaml", &["yaml.rs"]),
    ("malva", &["malva.rs"]),
    // oxc/config.rs: markup_fmt hands *its* table to the oxc formatter for
    // Astro `<script>` blocks, so those keys are read from this table too.
    ("markup_fmt", &["markup_fmt.rs", "oxc/config.rs"]),
    ("graphql", &["graphql.rs"]),
    ("biome", &["biome_css.rs", "biome_graphql.rs", "biome_common.rs"]),
    ("alejandra", &["nixfmt.rs"]),
    ("rubyfmt", &["rubyfmt.rs"]),
    ("hcl", &["hcl.rs"]),
    ("dockerfile", &["dockerfile.rs"]),
    ("dotenv", &["dotenv.rs"]),
    ("ini", &["ini.rs"]),
    ("treesitter", &["treesitter/mod.rs"]),
    ("polyconfig", &["config_keys/mod.rs"]),
    // The four cross-cutting backends read a table `Config` merged for them, so
    // their key lists are shared consts used by both sides of that merge.
    ("typos", &["typos.rs"]),
    ("quality", &["quality/settings.rs"]),
    ("uncomment", &["uncomment.rs"]),
    ("astgrep", &["astgrep/mod.rs"]),
    // Every wrapped toolchain binary shares one implementation and one key.
    ("gofmt", &["native_tool/mod.rs"]),
    ("rustfmt", &["native_tool/mod.rs"]),
    ("zigfmt", &["native_tool/mod.rs"]),
    ("shfmt", &["native_tool/mod.rs"]),
    ("shellcheck", &["native_tool/mod.rs"]),
    ("google-java-format", &["native_tool/mod.rs"]),
    ("ktfmt", &["native_tool/mod.rs"]),
    ("styler", &["native_tool/mod.rs"]),
    ("swift-format", &["native_tool/mod.rs"]),
    ("dartfmt", &["native_tool/mod.rs"]),
    ("gleamfmt", &["native_tool/mod.rs"]),
];

/// Keys a backend reads that no user ever writes: `Config` injects them into the
/// options table on the way in. They are reads without a declaration on purpose
/// — declaring them would tell users to write them.
const SYNTHETIC_KEYS: &[(&str, &[&str])] = &[("astgrep", &["rules_dirs", "rules_hash", "builtin_pack_enabled"])];

fn engines_source_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/engines")
}

fn source_of(engine: &str) -> String {
    let files = ENGINE_SOURCES
        .iter()
        .find(|(name, _)| *name == engine)
        .unwrap_or_else(|| panic!("no source files declared for engine `{engine}`"))
        .1;
    files
        .iter()
        .map(|file| std::fs::read_to_string(engines_source_dir().join(file)).expect("read engine source"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every engine the registry can hand a file to, once per name.
fn registry_engines() -> Vec<Box<dyn Engine>> {
    let mut seen = BTreeSet::new();
    let mut engines = Vec::new();
    for language in all_languages() {
        for engine in engines_for(&language) {
            if seen.insert(engine.name()) {
                engines.push(engine);
            }
        }
    }
    engines
}

#[test]
fn every_registry_engine_declares_its_option_keys() {
    for engine in registry_engines() {
        for table in [OptionTable::Lint, OptionTable::Format] {
            assert!(
                engine.option_keys(table).is_checked(),
                "`{}` leaves {table:?} unchecked: every key a user writes there would be accepted in silence",
                engine.name()
            );
        }
    }
}

#[test]
fn every_registry_engine_has_declared_sources() {
    for engine in registry_engines() {
        assert!(
            ENGINE_SOURCES.iter().any(|(name, _)| *name == engine.name()),
            "`{}` has no entry in ENGINE_SOURCES, so neither drift test covers it",
            engine.name()
        );
    }
}

/// Reader helpers whose *name* fixes the type of the key handed to them, when
/// the key is that call's first string argument.
const CALL_READERS: &[(&str, OptionType)] = &[
    ("usize_opt", OptionType::INTEGER),
    ("integer", OptionType::INTEGER),
    ("rule", OptionType::BOOLEAN),
    ("flag", OptionType::BOOLEAN),
    ("string_list_from_table", OptionType::ARRAY),
    ("string_list", OptionType::ARRAY),
    ("str_array", OptionType::ARRAY),
    ("str_list", OptionType::ARRAY),
];

/// Reader helpers taking a *second* key argument of a different type — the
/// quality tier's `rule(options, "<toggle>", "<threshold>", …)`.
const SECOND_ARG_READERS: &[(&str, OptionType)] = &[("rule", OptionType::INTEGER)];

/// `toml::Value` accessors, applied to the value a key resolves to. The
/// **first** one after the key is the type that key is read as.
const VALUE_ACCESSORS: &[(&str, OptionType)] = &[
    ("as_bool", OptionType::BOOLEAN),
    ("as_integer", OptionType::INTEGER),
    ("as_float", OptionType::FLOAT),
    ("as_str", OptionType::STRING),
    ("as_array", OptionType::ARRAY),
    ("as_table", OptionType::TABLE),
];

/// `toml::Value` *patterns*, matched against the value a key resolves to. Unlike
/// an accessor these legitimately come in sets — ruff matches both `Integer` and
/// `String` for `docstring_code_line_length` — so all of them count.
const VALUE_PATTERNS: &[(&str, OptionType)] = &[
    ("Value::Boolean", OptionType::BOOLEAN),
    ("Value::Integer", OptionType::INTEGER),
    ("Value::Float", OptionType::FLOAT),
    ("Value::String", OptionType::STRING),
    ("Value::Array", OptionType::ARRAY),
    ("Value::Table", OptionType::TABLE),
];

/// The type(s) `engine`'s own sources read `key` as, or `None` when no typed
/// read of it can be found.
///
/// Two shapes carry the answer, and every option read in the tree is one of
/// them: the key is an argument to a reader helper whose name fixes the type
/// (`flag(cfg, "code_only", …)`), or it is looked up and the resulting value is
/// then narrowed (`get("line_length").and_then(toml::Value::as_integer)`).
fn read_type_of(source: &str, key: &str) -> Option<OptionType> {
    // A helper call names its key *after* the reader, so look back — but only
    // within the same call, hence no parenthesis may intervene.
    let first_arg = Regex::new(&format!(
        r#"\b({})\s*\([^()"]*$"#,
        CALL_READERS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join("|")
    ))
    .expect("first-arg regex");
    let second_arg = Regex::new(&format!(
        r#"\b({})\s*\([^()]*?"[^"]*"\s*,\s*$"#,
        SECOND_ARG_READERS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join("|")
    ))
    .expect("second-arg regex");

    let needle = format!("\"{key}\"");
    let mut observed: Option<OptionType> = None;
    let mut widen = |found: OptionType| {
        observed = Some(observed.map_or(found, |seen: OptionType| seen.or(found)));
    };
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find(&needle) {
        let start = cursor + offset;
        let end = start + needle.len();
        cursor = end;
        let before = &source[..start];
        for (name, option_type) in SECOND_ARG_READERS {
            if second_arg.captures(before).is_some_and(|m| &m[1] == *name) {
                widen(*option_type);
            }
        }
        for (name, option_type) in CALL_READERS {
            if first_arg.captures(before).is_some_and(|m| &m[1] == *name) {
                widen(*option_type);
            }
        }
        // Forward: stop at the next string literal, so a later read of a
        // different key cannot be mistaken for this one's — and at the end of
        // the enclosing item (a `}` in column 0), so a *helper function's* own
        // accessor further down the file cannot be either.
        let after = &source[end..];
        let after = &after[..after.find('"').unwrap_or(after.len())];
        let after = &after[..after.find("\n}").map_or(after.len(), |at| at + 1)];
        match VALUE_ACCESSORS
            .iter()
            .filter_map(|(name, option_type)| after.find(name).map(|at| (at, *option_type)))
            .min_by_key(|(at, _)| *at)
        {
            // The first accessor is the narrowing; anything after it belongs to
            // the *elements*, as in `as_array` … `filter_map(as_integer)`.
            Some((_, option_type)) => widen(option_type),
            None => {
                for (name, option_type) in VALUE_PATTERNS {
                    if after.contains(name) {
                        widen(*option_type);
                    }
                }
            }
        }
    }
    observed
}

/// Every option key literal the backend's own sources read, found by scanning
/// for the call shapes an option read can take.
fn keys_read_by(engine: &str) -> BTreeSet<String> {
    let reader = Regex::new(
        r"(?s)(?:options\s*\.\s*(?:get|contains_key)|string_list(?:_from_table)?|str_array|sets_any|usize_opt|str_list|flag|integer|rule)\s*\(([^;]{0,200}?)\)",
    )
    .expect("reader regex");
    let literal = Regex::new(r#""([a-zA-Z][a-zA-Z0-9_\-]*)""#).expect("literal regex");
    let source = source_of(engine);
    let mut read = BTreeSet::new();
    for call in reader.captures_iter(&source) {
        for key in literal.captures_iter(&call[1]) {
            read.insert(key[1].to_string());
        }
    }
    read
}

#[test]
fn every_declared_option_key_is_one_the_backend_reads_at_the_declared_type() {
    // Direction one: a declaration naming a key the backend never reads is the
    // audited defect wearing a schema — the key still does nothing, and now
    // poly promises it does. Checked against the backend's actual read sites,
    // not merely against the text of its source, so writing the key into the
    // declaration cannot satisfy it.
    //
    // The *type* is checked the same way and for the same reason (issue #16):
    // `poly` now reports a value the declared type rejects, so a declaration
    // naming the wrong type turns a working setting into a false warning — the
    // one outcome this whole check exists to avoid. Every declared key must
    // therefore show a typed read, and the two must agree exactly.
    for engine in registry_engines() {
        let source = source_of(engine.name());
        let read = keys_read_by(engine.name());
        let synthetic = SYNTHETIC_KEYS
            .iter()
            .find(|(name, _)| *name == engine.name())
            .map(|(_, keys)| *keys)
            .unwrap_or(&[]);
        for table in [OptionTable::Lint, OptionTable::Format, OptionTable::CrossCuttingLint] {
            for (key, declared) in engine.option_keys(table).declared_keys() {
                if synthetic.contains(key) {
                    continue;
                }
                assert!(
                    read.contains(*key),
                    "`{}` declares `{key}` for {table:?}, but no read of that key was found in its sources",
                    engine.name()
                );
                let Some(found) = read_type_of(&source, key) else {
                    panic!(
                        "`{}` declares `{key}` as {} for {table:?}, but no typed read of it was found in its \
                         sources — an undeclared type is exactly the drift this check exists to prevent",
                        engine.name(),
                        declared.describe()
                    );
                };
                assert_eq!(
                    found,
                    *declared,
                    "`{}` declares `{key}` as {} for {table:?}, but its sources read it as {}",
                    engine.name(),
                    declared.describe(),
                    found.describe()
                );
            }
        }
    }
}

#[test]
fn every_option_key_a_backend_reads_is_declared() {
    // Direction two: a key the backend reads but does not declare would be
    // warned about while working perfectly — the false positive that makes a
    // warning untrustworthy.
    for engine in registry_engines() {
        let synthetic = SYNTHETIC_KEYS
            .iter()
            .find(|(name, _)| *name == engine.name())
            .map(|(_, keys)| *keys)
            .unwrap_or(&[]);
        let lint = engine.option_keys(OptionTable::Lint);
        let format = engine.option_keys(OptionTable::Format);
        let cross = engine.option_keys(OptionTable::CrossCuttingLint);
        let read = keys_read_by(engine.name());
        for key in read {
            if synthetic.contains(&key.as_str()) {
                continue;
            }
            // The derived probes answer about a *table*, so ask with one that
            // actually holds the key under test.
            let mut table = toml::Table::new();
            table.insert(key.clone(), toml::Value::Boolean(true));
            // Only *checked* tables count: an `UNCHECKED` one accepts
            // everything, and letting it answer here would make this assertion
            // vacuous for every backend that has one.
            let accepted = (lint.is_checked() && lint.accepts(&key, &table, true))
                || (format.is_checked() && format.accepts(&key, &table, true))
                || (cross.is_checked() && cross.accepts(&key, &table, false));
            assert!(
                accepted,
                "`{}` reads `{key}` but declares neither it nor a schema covering it",
                engine.name()
            );
        }
    }
}

#[test]
fn the_cross_cutting_merge_reads_the_keys_its_backend_declares() {
    // The four merged backends have their user-facing keys read in
    // `Config::build_*_options`, not in the backend, so the drift check above
    // cannot see that side. This pins the other one: every key the backend
    // declares survives the merge and arrives in the engine's options.
    for (engine, sample) in [
        ("typos", toml::Value::Array(vec![toml::Value::String("x".into())])),
        ("quality", toml::Value::Integer(7)),
        ("uncomment", toml::Value::Boolean(true)),
    ] {
        let declared = registry_engines()
            .iter()
            .find(|e| e.name() == engine)
            .expect("engine")
            .option_keys(OptionTable::CrossCuttingLint)
            .known_keys(false);
        for key in declared {
            // Maps and arrays and scalars all have to be offered in the shape
            // the merge expects, so try each until one survives.
            let candidates = [
                sample.clone(),
                toml::Value::Boolean(true),
                toml::Value::Integer(7),
                toml::Value::Array(vec![toml::Value::String("x".into())]),
                toml::Value::Table(toml::Table::from_iter([(
                    "teh".to_string(),
                    toml::Value::String("the".into()),
                )])),
            ];
            let survives = candidates.iter().any(|value| {
                let mut engine_table = toml::Table::new();
                engine_table.insert(key.to_string(), value.clone());
                let mut lint = toml::Table::new();
                lint.insert(engine.to_string(), toml::Value::Table(engine_table));
                let config = Config {
                    lint,
                    ..Config::default()
                };
                config
                    .engine_config(&Language::Python, engine, Kind::Lint)
                    .options
                    .contains_key(key)
            });
            assert!(
                survives,
                "`[lint.{engine}] {key}` is declared but does not survive Config's merge for {engine}"
            );
        }
    }
}

// ----------------------------------------------------------------- liveness

/// Build the engine config for one table of one language, through `Config` so
/// the merge in `config.rs` is exercised too.
fn engine_config(section: &str, language: &Language, engine: &str, source: &str) -> EngineConfig {
    let table: toml::Table = source.parse().expect("config");
    let mut config = Config::default();
    match section {
        "lint" => config.lint = table,
        _ => config.fmt = table,
    }
    let kind = if section == "lint" { Kind::Lint } else { Kind::Format };
    config.engine_config(language, engine, kind)
}

fn source_file(path: &str, language: Language, content: &str) -> SourceFile {
    SourceFile {
        path: PathBuf::from(path),
        language,
        content: content.into(),
    }
}

#[test]
fn a_declared_taplo_key_changes_the_formatted_output() {
    let src = source_file("a.toml", Language::Toml, "b = 1\naaa = 2\n");
    let engine = crate::engines::taplo::TaploEngine::new();
    let plain = engine_config("fmt", &Language::Toml, "taplo", "[toml.taplo]\n");
    let sorted = engine_config("fmt", &Language::Toml, "taplo", "[toml.taplo]\nreorder_keys = true\n");
    assert!(matches!(
        engine.format(&src, &plain).expect("fmt"),
        FormatOutput::Unchanged
    ));
    assert!(
        matches!(engine.format(&src, &sorted).expect("fmt"), FormatOutput::Formatted(text) if text.starts_with("aaa")),
        "`reorder_keys` is declared but does nothing"
    );
}

#[test]
fn a_declared_ruff_key_changes_the_formatted_output() {
    let src = source_file(
        "a.py",
        Language::Python,
        "values = [\"aaaaaaaaaaaaaaa\", \"bbbbbbbbbbbbbbb\", \"ccccccccccccccc\"]\n",
    );
    let engine = crate::engines::ruff::RuffEngine;
    let wide = engine_config("fmt", &Language::Python, "ruff", "[python.ruff]\nline_length = 120\n");
    let narrow = engine_config("fmt", &Language::Python, "ruff", "[python.ruff]\nline_length = 40\n");
    assert!(matches!(
        engine.format(&src, &wide).expect("fmt"),
        FormatOutput::Unchanged
    ));
    assert!(
        matches!(engine.format(&src, &narrow).expect("fmt"), FormatOutput::Formatted(text) if text.lines().count() > 1),
        "`line_length` is declared but does nothing"
    );
}

#[test]
fn a_declared_quality_key_changes_the_reported_diagnostics() {
    let body = (0..30)
        .map(|i| format!("    x{i} = {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let src = source_file("a.py", Language::Python, &format!("def f():\n{body}\n"));
    let engine = crate::engines::quality::QualityEngine;
    let lenient = engine_config("lint", &Language::Python, "quality", "[quality]\n");
    let strict = engine_config(
        "lint",
        &Language::Python,
        "quality",
        "[quality]\nfunction_too_long_lines = 5\n",
    );
    assert!(engine.lint(&src, &lenient).expect("lint").is_empty());
    assert!(
        !engine.lint(&src, &strict).expect("lint").is_empty(),
        "`function_too_long_lines` is declared but does nothing"
    );
}

#[test]
fn a_declared_oxc_key_changes_the_formatted_output() {
    let src = source_file("a.js", Language::JavaScript, "const a = \"x\";\n");
    let engine = crate::engines::oxc::OxcEngine;
    let double = engine_config("fmt", &Language::JavaScript, "oxc", "[javascript.oxc]\n");
    let single = engine_config(
        "fmt",
        &Language::JavaScript,
        "oxc",
        "[javascript.oxc]\nquote_style = \"single\"\n",
    );
    assert!(matches!(
        engine.format(&src, &double).expect("fmt"),
        FormatOutput::Unchanged
    ));
    assert!(
        matches!(engine.format(&src, &single).expect("fmt"), FormatOutput::Formatted(text) if text.contains('\'')),
        "`quote_style` is declared but does nothing"
    );
}

#[test]
fn the_check_is_wired_into_the_registry_for_toml() {
    // Without this the whole feature is inert: every test above calls the
    // backend directly, and none of them would notice it never runs.
    assert!(
        engines_for(&Language::Toml)
            .iter()
            .any(|engine| engine.name() == "polyconfig"),
        "the poly.toml schema backend is not registered for TOML"
    );
}
