//! The web engines' option declarations are honest, and the keys a user writes in `poly.toml` reach them.
//!
//! The root workspace's `crates/poly-core/src/engines/config_keys/tests.rs` runs the same checks over
//! the engines `poly-core` links; the web family lives here, so its drift tests do too.
//!
//! 1. **Honesty of the declarations** - every declared key is one the engine's own source reads *at the
//!    declared type*, and every option read in an engine's source is declared.
//! 2. **Behaviour** - serde-derived tables (malva, markup_fmt) recognise the keys their upstream type
//!    does, through `poly.toml` key checking with the provider registered.
//! 3. **Liveness** - setting a declared key changes what the engine produces.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use poly_core::config::{Config, Kind};
use poly_core::engines::config_keys::PolyConfigEngine;
use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::engine::{Diagnostic, Engine, FormatOutput, OptionTable, OptionType, SourceFile};
use poly_engine_api::language::Language;
use poly_engines_web::{OxcEngine, TaploEngine, engines_for};
use regex::Regex;

mod common;

/// Where each web engine's option reads live, so the drift tests can compare a declaration against the
/// code it claims to describe. Paths are relative to this crate's `src/`.
const ENGINE_SOURCES: &[(&str, &[&str])] = &[
    (
        "oxc",
        &[
            "oxc/mod.rs",
            "oxc/config.rs",
            "oxc/lint.rs",
            "oxc/format.rs",
            "oxc/plugins.rs",
            "oxc/sort_imports.rs",
            "oxc/native_lint.rs",
            "oxc/external.rs",
            "core_options.rs",
            // The `oxlintrc` read is a host hook shared with the component host.
            "../../../../crates/poly-engine-api/src/host_hooks/oxlintrc.rs",
        ],
    ),
    (
        "rumdl",
        &["rumdl.rs", "markdown_fmt.rs", "markdown_embed.rs", "core_options.rs"],
    ),
    ("taplo", &["taplo.rs", "core_options.rs"]),
    ("yaml", &["yaml.rs", "core_options.rs"]),
    ("malva", &["malva.rs", "core_options.rs"]),
    // oxc/config.rs: markup_fmt hands *its* table to the oxc formatter for
    // Astro `<script>` blocks, so those keys are read from this table too.
    (
        "markup_fmt",
        &[
            "markup_fmt.rs",
            "oxc/config.rs",
            "oxc/sort_imports.rs",
            "core_options.rs",
        ],
    ),
    ("adrlint", &["adrlint.rs", "adrlint/project.rs"]),
];

/// Languages whose engines together cover every web engine.
const LANGUAGES: &[Language] = &[
    Language::JavaScript,
    Language::Toml,
    Language::Markdown,
    Language::Mdx,
    Language::Yaml,
    Language::Css,
    Language::Html,
];

fn engines_source_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
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

/// Every web engine, once per name.
fn web_engines() -> Vec<Box<dyn Engine>> {
    let mut seen = BTreeSet::new();
    let mut engines = Vec::new();
    for language in LANGUAGES {
        for engine in engines_for(language) {
            if seen.insert(engine.name()) {
                engines.push(engine);
            }
        }
    }
    engines
}

/// Lint a `poly.toml` source through the key-checking backend, exactly as the runner would. The web
/// provider is registered first so serde-derived tables resolve their probes.
fn warnings(source: &str) -> Vec<Diagnostic> {
    common::register();
    let src = SourceFile {
        path: PathBuf::from("poly.toml"),
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

#[test]
fn every_web_engine_declares_its_option_keys() {
    for engine in web_engines() {
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
fn every_web_engine_has_declared_sources() {
    let engines = web_engines();
    assert_eq!(engines.len(), 7, "every web engine is reached by a sample language");
    for engine in engines {
        assert!(
            ENGINE_SOURCES.iter().any(|(name, _)| *name == engine.name()),
            "`{}` has no entry in ENGINE_SOURCES, so neither drift test covers it",
            engine.name()
        );
    }
}

// ------------------------------------------------- honesty of declarations

/// Reader helpers whose *name* fixes the type of the key handed to them, when
/// the key is that call's first string argument.
const CALL_READERS: &[(&str, OptionType)] = &[
    ("option_str", OptionType::STRING),
    ("option_bool", OptionType::BOOLEAN),
    ("usize_opt", OptionType::INTEGER),
    ("integer", OptionType::INTEGER),
    ("rule", OptionType::BOOLEAN),
    ("flag", OptionType::BOOLEAN),
    ("string_list_from_table", OptionType::ARRAY),
    ("string_list", OptionType::ARRAY),
    ("str_array", OptionType::ARRAY),
    ("str_list", OptionType::ARRAY),
    // ruff's `require-lazy` / `ban-lazy`: `"all"`, a list, or an `{ include, exclude }` table.
    (
        "import_selector",
        OptionType::STRING.or(OptionType::ARRAY).or(OptionType::TABLE),
    ),
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
        r"(?s)(?:options\s*\.\s*(?:get|contains_key)|option_str|option_bool|string_list(?:_from_table)?|str_array|sets_any|usize_opt|str_list|import_selector|flag|integer|rule)\s*\(([^;]{0,200}?)\)",
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

/// Option keys an engine hands to the host through `project_inputs` instead of reading itself.
fn host_read_keys(engine: &dyn Engine) -> BTreeSet<String> {
    let inputs = engine.project_inputs();
    let mut keys = BTreeSet::new();
    keys.extend(inputs.gate_key);
    keys.extend(inputs.files.into_iter().map(|file| file.option_key));
    if let Some(dirs) = inputs.dirs {
        keys.insert(dirs.dir_key);
        keys.extend(dirs.paths_key);
        keys.extend(dirs.recursive_key);
    }
    keys
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
    for engine in web_engines() {
        let source = source_of(engine.name());
        let read = keys_read_by(engine.name());
        let host_read = host_read_keys(engine.as_ref());
        for table in [OptionTable::Lint, OptionTable::Format, OptionTable::CrossCuttingLint] {
            for (key, declared) in engine.option_keys(table).declared_keys() {
                // Keys named in `project_inputs` are read by the host, which loads the files
                // they select; the backend never sees the option, only the files it names.
                if host_read.contains(*key) {
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
    for engine in web_engines() {
        let lint = engine.option_keys(OptionTable::Lint);
        let format = engine.option_keys(OptionTable::Format);
        let cross = engine.option_keys(OptionTable::CrossCuttingLint);
        let read = keys_read_by(engine.name());
        for key in read {
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

// ------------------------------------------------------------- behaviour

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
    let engine = engines_for(&Language::Html)
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
        let declared_accepts_boolean = keys
            .expected_type(key, true)
            .is_none_or(|declared| declared.accepts(&table[*key]));
        let recognized_upstream = keys.accepts(key, &table, true);
        assert!(
            recognized_upstream,
            "`{key}` must stay an accepted key of `[fmt.html.markup_fmt]`"
        );
        assert_eq!(
            reported, !declared_accepts_boolean,
            "`{key}`: a boolean is reported exactly when the declaration says the key is not boolean"
        );
    }
}

// ----------------------------------------------------------------- liveness

/// Build the engine config for one table of one language, through `Config` so the merge in `config.rs`
/// is exercised too.
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
    let engine = TaploEngine::new();
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
fn a_declared_oxc_key_changes_the_formatted_output() {
    let src = source_file("a.js", Language::JavaScript, "const a = \"x\";\n");
    let engine = OxcEngine;
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
