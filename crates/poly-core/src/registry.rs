//! Backend registry: maps a [`Language`] to the ordered list of engines that
//! handle it. Native backends are wired here as they land; the
//! [`TreeSitterEngine`] generic tier serves any language no native backend has
//! claimed (structural reindent for brace grammars, whitespace normalization
//! otherwise).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, OnceLock};

use crate::engine::Engine;
use crate::engines::config_keys::PolyConfigEngine;
use crate::engines::native_tool::NativeToolEngine;
#[cfg(feature = "engine-quality")]
use crate::engines::quality::QualityEngine;
use crate::engines::treesitter::TreeSitterEngine;
use crate::language::Language;

/// A function supplying engines for a language from outside `poly-core`.
///
/// Returns an empty list for languages it does not serve.
pub type EngineProvider = fn(&Language) -> Vec<Box<dyn Engine>>;

static EXTERNAL_PROVIDER: OnceLock<EngineProvider> = OnceLock::new();

/// Set once [`engine_versions`] has built its process-wide table, after which a provider could no
/// longer contribute to it.
static VERSIONS_BUILT: AtomicBool = AtomicBool::new(false);

/// Name of the generic tree-sitter engine, the tier-2 formatting fallback for languages with no
/// dedicated backend. Matched by name so it can be dropped when another formatter takes over.
pub(crate) const TREE_SITTER_ENGINE: &str = "treesitter";

/// Why [`register_engine_provider`] refused a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRegistrationError {
    /// A provider is already registered; the first registration wins.
    AlreadyRegistered,
    /// The registry's process-wide engine table was already built without a provider.
    TooLate,
}

impl std::fmt::Display for ProviderRegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRegistered => f.write_str("an engine provider is already registered"),
            Self::TooLate => f.write_str("the engine registry was already consulted; register providers at startup"),
        }
    }
}

impl std::error::Error for ProviderRegistrationError {}

/// Registers the process-wide external engine provider.
///
/// Meant for a bundled binary linking engines `poly-core` does not carry (the git-pinned
/// oxc, biome and rubyfmt families): call it once at startup, before the first lint or format
/// run. The provider's engines are placed among the language's built-in engines (before any lint-only built-in) and before the
/// cross-cutting ones. When one of them formats the language, the tree-sitter generic formatter
/// is dropped for it so the two never compete over indentation; lint-only engines leave it in
/// place. Provider engine names must not collide with the language's built-in engine names.
///
/// # Errors
///
/// [`ProviderRegistrationError::AlreadyRegistered`] if a provider is already set (first wins), and
/// [`ProviderRegistrationError::TooLate`] if [`engine_versions`] has already been built, since
/// that table would silently omit the provider's engines.
pub fn register_engine_provider(provider: EngineProvider) -> Result<(), ProviderRegistrationError> {
    if VERSIONS_BUILT.load(Ordering::Acquire) {
        return Err(ProviderRegistrationError::TooLate);
    }
    EXTERNAL_PROVIDER
        .set(provider)
        .map_err(|_| ProviderRegistrationError::AlreadyRegistered)
}

/// Whether any of `engines` formats a language it serves, and so displaces the tree-sitter
/// reindenter. Shared by the planner (component engines) and the registry (external provider).
pub(crate) fn displaces_tree_sitter<'a>(engines: impl IntoIterator<Item = &'a dyn Engine>) -> bool {
    engines
        .into_iter()
        .any(|engine| engine.capabilities().format && !engine.languages().is_empty())
}

/// Merges engines contributed from outside the registry (a provider's, or a downloadable
/// component's) into a language's built-in engines.
///
/// When one of them formats the language the tree-sitter reindenter is dropped. The order is the
/// one the engines had when they were all built in. A contributed language engine goes before the
/// first built-in engine that cannot format (a lint-only core engine such as `PolyConfigEngine`):
/// TOML is `[Taplo, PolyConfig]`, not `[PolyConfig, Taplo]`; among themselves the contributed
/// formatters come first. A contributed cross-cutting engine
/// (one claiming no languages, such as a component-served `typos`) goes before the first built-in
/// cross-cutting one, since the registry appends those after every language engine.
pub(crate) fn merge_external_engines(
    mut engines: Vec<Box<dyn Engine>>,
    external: Vec<Box<dyn Engine>>,
) -> Vec<Box<dyn Engine>> {
    if external.is_empty() {
        return engines;
    }
    if displaces_tree_sitter(external.iter().map(|engine| &**engine)) {
        engines.retain(|engine| engine.name() != TREE_SITTER_ENGINE);
    }
    let (mut cross_cutting, mut language): (Vec<_>, Vec<_>) =
        external.into_iter().partition(|engine| engine.languages().is_empty());
    // The bundled build appends cross-cutting engines in `CROSS_CUTTING_ENGINES` order; an unknown
    // one keeps its arrival order after them.
    cross_cutting.sort_by_key(|engine| {
        CROSS_CUTTING_ENGINES
            .iter()
            .position(|name| *name == engine.name())
            .unwrap_or(CROSS_CUTTING_ENGINES.len())
    });
    // Component engines arrive in index order (by name), not the family's own: a language's
    // formatter leads its lint-only engines in every family (Markdown `[rumdl, adrlint]`, GraphQL
    // `[graphql, biome]`), so a stable sort on that restores it.
    language.sort_by_key(|engine| !engine.capabilities().format);
    let at = engines
        .iter()
        .position(|engine| !engine.capabilities().format)
        .unwrap_or(engines.len());
    engines.splice(at..at, language);
    let at = engines
        .iter()
        .position(|engine| engine.languages().is_empty() && engine.name() != TREE_SITTER_ENGINE)
        .unwrap_or(engines.len());
    engines.splice(at..at, cross_cutting);
    engines
}

/// Merges the external provider's engines for `lang` into the built-in ones.
fn with_external_engines(lang: &Language, engines: Vec<Box<dyn Engine>>) -> Vec<Box<dyn Engine>> {
    match EXTERNAL_PROVIDER.get() {
        Some(provider) => {
            let external = provider(lang);
            debug_assert!(
                external
                    .iter()
                    .all(|new| engines.iter().all(|existing| existing.name() != new.name())),
                "an external engine provider supplied an engine whose name collides with a built-in one for {lang:?}"
            );
            merge_external_engines(engines, external)
        }
        None => engines,
    }
}

/// Engines applicable to a language, in priority order (formatters run in sequence).
///
/// Every arm that names an engine family is gated on that family's `engine-*` feature. With the
/// family off the arm disappears and its languages reach the final `_ => TreeSitterEngine` arm,
/// the same tier any language without a native backend gets. The web and biome families are not
/// here: their engines come from the `web` and `biome` components or a bundled binary's provider.
pub fn engines_for(lang: &Language) -> Vec<Box<dyn Engine>> {
    if matches!(lang, Language::Other(name) if name == crate::language::HELM_TEMPLATE) {
        return Vec::new();
    }
    let engines: Vec<Box<dyn Engine>> = match lang {
        // `poly.toml` key checking is core; the TOML formatter and linter are the web family's.
        Language::Toml => vec![Box::new(TreeSitterEngine), Box::new(PolyConfigEngine)],
        #[cfg(feature = "engine-python")]
        Language::Python => poly_engines_python::engines_for(lang),
        #[cfg(feature = "engine-sql")]
        Language::Sql => poly_engines_sql::engines_for(lang),
        #[cfg(feature = "engine-nix")]
        Language::Nix => poly_engines_nix::engines_for(lang),
        #[cfg(feature = "engine-graphql")]
        Language::GraphQl => poly_engines_misc::engines_for(lang),
        #[cfg(feature = "engine-hcl")]
        Language::Hcl => poly_engines_misc::engines_for(lang),
        #[cfg(feature = "engine-php")]
        Language::Php => poly_engines_php::engines_for(lang),
        #[cfg(feature = "engine-dockerfile")]
        Language::Dockerfile => poly_engines_misc::engines_for(lang),
        #[cfg(feature = "engine-dotenv")]
        Language::Dotenv => poly_engines_misc::engines_for(lang),
        #[cfg(feature = "engine-ini")]
        Language::Ini => poly_engines_misc::engines_for(lang),
        Language::Go => vec![Box::new(NativeToolEngine::for_language(Language::Go))],
        Language::Rust => vec![Box::new(NativeToolEngine::for_language(Language::Rust))],
        Language::Zig => vec![Box::new(NativeToolEngine::for_language(Language::Zig))],
        Language::Java => vec![Box::new(NativeToolEngine::for_language(Language::Java))],
        Language::Kotlin => vec![Box::new(NativeToolEngine::for_language(Language::Kotlin))],
        Language::R => vec![Box::new(NativeToolEngine::for_language(Language::R))],
        Language::Swift => vec![Box::new(NativeToolEngine::for_language(Language::Swift))],
        Language::Dart => vec![Box::new(NativeToolEngine::for_language(Language::Dart))],
        Language::Gleam => vec![Box::new(NativeToolEngine::for_language(Language::Gleam))],
        Language::Shell => vec![
            Box::new(NativeToolEngine::shell_format()),
            Box::new(NativeToolEngine::shell_lint()),
        ],
        _ => vec![Box::new(TreeSitterEngine)],
    };
    // `mut` is unused when every cross-cutting family is compiled out.
    #[allow(unused_mut)]
    let mut engines = with_external_engines(lang, engines);
    #[cfg(feature = "engine-typos")]
    engines.extend(poly_engines_typos::engines_for(lang));
    #[cfg(feature = "engine-astgrep")]
    engines.extend(poly_engines_astgrep::engines_for(lang));
    #[cfg(feature = "engine-uncomment")]
    engines.extend(poly_engines_uncomment::engines_for(lang));
    #[cfg(feature = "engine-quality")]
    engines.push(Box::new(QualityEngine));
    engines
}

/// One instance of every engine [`engines_for`] registers, across every language (duplicates
/// included: an engine serving several languages appears once per language).
///
/// Exists for the compiled component index generator and its drift test, which describe the
/// registered engines without a second hand-maintained list.
#[doc(hidden)]
pub fn registered_engines() -> Vec<Box<dyn Engine>> {
    all_languages().iter().flat_map(engines_for).collect()
}

/// Every engine name the registry can plan, across every language.
///
/// Walks [`all_languages`] rather than holding a second hand-maintained list,
/// so a backend wired into [`engines_for`] is reachable here the moment it is
/// registered. Callers that also accept catalog tools must add those
/// themselves — the catalog tier is configured, not registered.
pub(crate) fn all_engine_names() -> std::collections::BTreeSet<&'static str> {
    all_languages()
        .iter()
        .flat_map(|language| engines_for(language).into_iter().map(|engine| engine.name()))
        .collect()
}

/// Ids of the languages `engine` is registered for in the lint (`format == false`) or format
/// (`format == true`) phase.
///
/// Walks [`all_languages`] through [`engines_for`], so it answers what the planner would do, with
/// the same feature gating; an engine none of whose family is linked is answered from the compiled
/// component index instead. Cross-cutting engines are registered for every language and say so here. Used by `poly migrate` to hoist a
/// per-language option to engine level only when every served language carries it.
pub fn engine_languages(engine: &str, format: bool) -> Vec<String> {
    let linked: Vec<String> = all_languages()
        .iter()
        .filter(|language| {
            engines_for(language).iter().any(|candidate| {
                let capabilities = candidate.capabilities();
                candidate.name() == engine && if format { capabilities.format } else { capabilities.lint }
            })
        })
        .map(|language| language.id().to_string())
        .collect();
    if !linked.is_empty() {
        return linked;
    }
    // An engine this binary does not link (its family is a component) is answered from the compiled
    // component index, so what `poly migrate` hoists does not depend on which families are linked.
    let served: std::collections::BTreeSet<&str> = poly_component_index::index()
        .engine_descs(engine)
        .filter(|desc| {
            if format {
                desc.capabilities.format
            } else {
                desc.capabilities.lint
            }
        })
        .flat_map(|desc| desc.languages.iter().map(String::as_str))
        .collect();
    all_languages()
        .iter()
        .filter(|language| served.contains(language.id()))
        .map(|language| language.id().to_string())
        .collect()
}

/// Engines that wrap a **host** toolchain rather than a compiled-in crate.
///
/// Their `version()` probes `PATH` and fingerprints repo-local tool config, so
/// it differs between machines *and* between checkouts on one machine. Anything
/// that reports a process-constant identity must leave them out, and the
/// stale-cache audit skips them because there is no lock entry to track.
pub(crate) const NATIVE_TOOLCHAIN_ENGINES: &[&str] = &[
    "gofmt",
    "rustfmt",
    "zigfmt",
    "shfmt",
    "shellcheck",
    "google-java-format",
    "ktfmt",
    "styler",
    "swift-format",
    "dartfmt",
    "gleamfmt",
];

/// Every compiled-in engine's name paired with its `version()` string.
///
/// Built once per process. This is the identity of what poly *is* — the set of
/// backends baked into the binary and the upstream versions they wrap — which a
/// caller needs to tell two clean reports apart when the binary is the same but
/// its engines are not.
///
/// **Deliberately partial, in two ways that matter.** Native-toolchain backends
/// ([`NATIVE_TOOLCHAIN_ENGINES`]) are excluded because their `version()` probes
/// `PATH` and fingerprints repo-local config, so it is neither process-constant
/// nor machine-independent — folding it in would make this identity change
/// between two checkouts on one machine. Catalog-tier engines are absent because
/// they do not exist until a config enables them, so they are not a property of
/// the binary at all.
pub fn engine_versions() -> &'static BTreeMap<&'static str, String> {
    static VERSIONS: LazyLock<BTreeMap<&'static str, String>> = LazyLock::new(|| {
        VERSIONS_BUILT.store(true, Ordering::Release);
        all_languages()
            .iter()
            .flat_map(|language| engines_for(language))
            .filter(|engine| !NATIVE_TOOLCHAIN_ENGINES.contains(&engine.name()))
            .map(|engine| (engine.name(), engine.version().to_owned()))
            .collect()
    });
    &VERSIONS
}

/// The backends [`engines_for`] appends to *every* language, named.
///
/// These are the only engines configurable from a language-agnostic
/// `[<kind>.<engine>]` table, so `Config::engine_config` needs them by name to
/// know where a global setting for one of them may live. Kept honest by
/// `tests::cross_cutting_engines_are_the_ones_claiming_no_languages`, which
/// derives the same set from `languages()` rather than trusting this list.
///
/// Not feature-gated: a family left out of the build is served by a component, and its engine
/// reads the same tables.
pub(crate) const CROSS_CUTTING_ENGINES: &[&str] = &["typos", "astgrep", "uncomment", "quality"];

/// Engines that are planned only when their `enabled` option is explicitly `true`.
///
/// Every other engine runs unless switched off. An absent key on one of these means
/// "off", so a project that never mentions `typos` neither spell-checks nor loads it.
/// `uncomment` is listed so a core build, which serves it from the `uncomment` component, never
/// fetches that component for a project that has not enabled it; it also gates itself inside `lint`.
/// Not feature-gated, like [`CROSS_CUTTING_ENGINES`]: a component-served typos is opt-in too.
pub(crate) const OPT_IN_ENGINES: &[&str] = &["typos", "uncomment"];

/// Every concrete (non-[`Language::Other`]) [`Language`] variant, used to
/// walk every arm of [`engines_for`]. Kept from silently narrowing by
/// `tests::assert_all_language_variants_listed`: adding a new `Language`
/// variant without adding it to both that match and this list fails to
/// compile, rather than letting a newly registry-wired engine for the new
/// language slip past this audit unnoticed.
///
/// Shared with `runner::plan`'s tests, which walk the same list one level
/// up (registry engines *plus* catalog tools); a second hand-maintained copy
/// there could narrow without this file's compile-time guard noticing.
pub(crate) fn all_languages() -> Vec<Language> {
    vec![
        Language::Python,
        Language::JavaScript,
        Language::TypeScript,
        Language::Jsx,
        Language::Tsx,
        Language::Json,
        Language::Jsonc,
        Language::Yaml,
        Language::Toml,
        Language::Markdown,
        Language::Mdx,
        Language::Sql,
        Language::Css,
        Language::Scss,
        Language::Less,
        Language::Html,
        Language::Vue,
        Language::Svelte,
        Language::Astro,
        Language::Angular,
        Language::Jinja,
        Language::Vento,
        Language::Mustache,
        Language::Xml,
        Language::GraphQl,
        Language::Hcl,
        Language::Nix,
        Language::Shell,
        Language::Dockerfile,
        Language::Go,
        Language::Java,
        Language::Kotlin,
        Language::Ruby,
        Language::Php,
        Language::R,
        Language::Elixir,
        Language::C,
        Language::Cpp,
        Language::Rust,
        Language::Proto,
        Language::Zig,
        Language::Swift,
        Language::Dart,
        Language::Gleam,
        Language::CSharp,
        Language::Dotenv,
        Language::Ini,
    ]
}

#[cfg(test)]
pub(crate) mod tests {
    //! Structural guard against the failure family described in
    //! `tests/version_audit.rs`: that test hand-lists which engines it checks
    //! against `Cargo.lock`, and `astgrep` was once left off that list —
    //! `astgrep` kept advertising a stale `tree-sitter-language-pack` version
    //! while the audit stayed green, because nothing forced the list to stay
    //! exhaustive. `engines_for` is `pub(crate)`, so `tests/version_audit.rs`
    //! (an external integration-test binary that only sees the public API)
    //! cannot walk it directly — this in-crate unit test does, and cross-checks
    //! the result against what `version_audit.rs` actually declares.
    use std::collections::BTreeSet;
    use std::path::Path;

    use regex::Regex;

    use super::{all_languages, engines_for};

    /// Test-module alias for [`all_languages`], kept because `runner::plan`'s
    /// tests reach for it by this path.
    pub(crate) fn all_known_languages() -> Vec<Language> {
        all_languages()
    }
    use crate::language::Language;

    /// Compile-time companion to `all_languages`: an exhaustive match
    /// (no wildcard arm) over every `Language` variant. Never called — its only
    /// purpose is that adding a variant to the enum without adding it here
    /// stops this file compiling, forcing whoever adds it to also decide
    /// whether `all_languages` (and therefore this audit) needs it.
    #[allow(dead_code)]
    fn assert_all_language_variants_listed(language: &Language) {
        match language {
            Language::Python
            | Language::JavaScript
            | Language::TypeScript
            | Language::Jsx
            | Language::Tsx
            | Language::Json
            | Language::Jsonc
            | Language::Yaml
            | Language::Toml
            | Language::Markdown
            | Language::Mdx
            | Language::Sql
            | Language::Css
            | Language::Scss
            | Language::Less
            | Language::Html
            | Language::Vue
            | Language::Svelte
            | Language::Astro
            | Language::Angular
            | Language::Jinja
            | Language::Vento
            | Language::Mustache
            | Language::Xml
            | Language::GraphQl
            | Language::Hcl
            | Language::Nix
            | Language::Shell
            | Language::Dockerfile
            | Language::Go
            | Language::Java
            | Language::Kotlin
            | Language::Ruby
            | Language::Php
            | Language::R
            | Language::Elixir
            | Language::C
            | Language::Cpp
            | Language::Rust
            | Language::Proto
            | Language::Zig
            | Language::Swift
            | Language::Dart
            | Language::Gleam
            | Language::CSharp
            | Language::Dotenv
            | Language::Ini
            | Language::Other(_) => {}
        }
    }

    /// Native-toolchain backends (`native_tool.rs`) wrap an external
    /// first-party CLI (`gofmt`, `rustfmt`, …), not a pinned Rust crate, so
    /// `tests/version_audit.rs` has no `Cargo.lock` entry to check them
    /// against. This is the one visible, explicit exemption list — engines
    /// land here on purpose, never by being left off both lists.
    ///
    /// Catalog backends (`engines/catalog_tool`) are the other legitimately
    /// exempt family, wrapping arbitrary user-configured external tools; they
    /// never appear in `engines_for` at all (they are built from `poly.toml`
    /// in `runner/plan.rs`, not wired into the registry), so they cannot reach
    /// this traversal and need no entry here.
    /// Backends that wrap nothing at all: their behaviour is poly's own code,
    /// so there is no upstream version for `tests/version_audit.rs` to check.
    /// `polyconfig`'s `version()` tracks poly's own config-key schema instead.
    const POLY_OWNED_ENGINES: &[&str] = &["polyconfig"];

    /// `Config::engine_config` resolves a cross-cutting engine's global
    /// `enabled` by reading `[<kind>.<engine>]` out of the same table that holds
    /// the per-language subtables keyed by `Language::id()`. That is only
    /// unambiguous while no language shares a name with a cross-cutting engine —
    /// an assumption nothing enforced until this test, and one a new language
    /// (or a renamed engine) could quietly break.
    #[test]
    fn no_language_id_collides_with_a_cross_cutting_engine_name() {
        for language in all_languages() {
            assert!(
                !super::CROSS_CUTTING_ENGINES.contains(&language.id()),
                "language id {:?} collides with a cross-cutting engine name; \
                 `Config::engine_config` would read one table as the other",
                language.id(),
            );
        }
    }

    /// [`CROSS_CUTTING_ENGINES`] is a hand-written list, so derive the same set
    /// from the property that actually defines it — an engine claiming no
    /// languages — and compare. A backend added to the append block without a
    /// name here would otherwise be silently unreachable from a global
    /// `[lint.<engine>]` table.
    #[test]
    fn cross_cutting_engines_are_the_ones_claiming_no_languages() {
        // Go, not Python: Python falls through to the (language-less) tree-sitter tier when `engine-python` is off.
        // A family this build does not link is served by a component, so its engines come from
        // the compiled component index instead.
        let mut derived: Vec<&str> = engines_for(&Language::Go)
            .iter()
            .filter(|engine| engine.languages().is_empty())
            .map(|engine| engine.name())
            .collect();
        let index = poly_component_index::index();
        derived.extend(
            index
                .components
                .iter()
                .flat_map(|component| &component.engines)
                .filter(|desc| desc.languages.is_empty())
                .map(|desc| desc.name.as_str()),
        );
        derived.sort_unstable();
        derived.dedup();
        let mut listed = super::CROSS_CUTTING_ENGINES.to_vec();
        listed.sort_unstable();
        assert_eq!(derived, listed);
    }

    use super::NATIVE_TOOLCHAIN_ENGINES;

    #[cfg(feature = "engine-astgrep")]
    /// The guard on `GRAMMAR_FOR_LANGUAGE_ID`: every language poly can detect
    /// must reach a grammar ast-grep can parse with, or that language silently
    /// has no custom-rule coverage at all. `all_languages` is itself held
    /// exhaustive by a compile-time match in `registry`, so a newly added
    /// `Language` variant reaches this assertion.
    #[test]
    fn every_language_id_resolves_to_a_grammar() {
        for language in all_languages() {
            let grammar = poly_engines_astgrep::language::grammar_for_language_id(language.id());
            assert!(
                poly_engines_astgrep::language::TslpLanguage::new(grammar).is_some(),
                "{} maps to grammar {grammar:?}, which tree-sitter-language-pack does not ship",
                language.id()
            );
        }
    }

    /// Engine names `tests/version_audit.rs` declares via `check("name", ...)`.
    /// Read from the sibling file's source text rather than duplicating the
    /// list here, so the two files cannot drift apart.
    fn audited_engine_names() -> BTreeSet<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/version_audit.rs");
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let pattern = Regex::new(r#"check\(\s*"([^"]+)""#).expect("valid regex");
        pattern
            .captures_iter(&source)
            .map(|capture| capture[1].to_owned())
            .collect()
    }

    #[test]
    fn every_registered_engine_is_audited_or_declared_exempt() {
        let mut registered: BTreeSet<&'static str> = BTreeSet::new();
        for language in all_languages() {
            for engine in engines_for(&language) {
                registered.insert(engine.name());
            }
        }
        assert!(
            !registered.is_empty(),
            "collected zero engines from the registry; the traversal is broken, not the audit"
        );

        let audited = audited_engine_names();
        assert!(
            !audited.is_empty(),
            "collected zero `check(...)` entries from tests/version_audit.rs; the \
             sibling-file scan is broken, not the audit"
        );

        for name in &registered {
            let is_exempt = NATIVE_TOOLCHAIN_ENGINES.contains(name) || POLY_OWNED_ENGINES.contains(name);
            let is_audited = audited.contains(*name);
            assert!(
                is_exempt || is_audited,
                "engine `{name}` is wired into `registry::engines_for` but is neither \
                 checked in tests/version_audit.rs nor declared exempt in \
                 registry::tests::NATIVE_TOOLCHAIN_ENGINES. If it wraps a pinned Rust crate, \
                 add a `check(\"{name}\", ...)` entry naming that crate; if it wraps an \
                 external CLI with no pinned crate, add \"{name}\" to \
                 NATIVE_TOOLCHAIN_ENGINES instead; if it wraps nothing, add it to \
                 POLY_OWNED_ENGINES.",
            );
        }
    }

    /// Two engines may share a name only while they never serve the same
    /// language. Everything that keys off `Engine::name()` — the
    /// `[<kind>.<lang>.<engine>]` config table, the per-plan severity remap, the
    /// cache id — is scoped by language first, so a shared name is safe while the
    /// engines sharing it serve disjoint languages (the biome pair, checked in
    /// `poly-engines-biome`'s `tests/identity.rs`).
    ///
    /// If this fails, either give the colliding backends distinct names — and
    /// bump both `version()` strings, so entries cached under the old name are
    /// invalidated rather than silently reused — or keep their languages
    /// disjoint.
    ///
    /// This checks the registry only. Catalog tools (ADR 0013) join the very
    /// same list in `runner::plan::plan_engines`, so they can collide with a
    /// registry backend by name too; the guard that covers both is
    /// `runner::plan::tests::planned_engine_names_are_unique_per_language_and_kind`.
    #[test]
    fn registered_engine_names_are_unique_per_language() {
        let languages = all_languages()
            .into_iter()
            .chain(std::iter::once(Language::Other("elm".to_owned())));
        for language in languages {
            let engines = engines_for(&language);
            let mut seen: Vec<&'static str> = Vec::with_capacity(engines.len());
            for engine in &engines {
                let name = engine.name();
                assert!(
                    !seen.contains(&name),
                    "two engines registered for {language:?} both answer the name {name:?}; \
                     they would share one config table and one cache id",
                );
                seen.push(name);
            }
        }
    }

    /// A fake engine with a name, a language set and whether it formats.
    struct Fake(&'static str, &'static [Language], bool);

    impl crate::engine::Engine for Fake {
        fn name(&self) -> &'static str {
            self.0
        }

        fn languages(&self) -> &'static [Language] {
            self.1
        }

        fn capabilities(&self) -> crate::engine::Capabilities {
            crate::engine::Capabilities {
                lint: true,
                format: self.2,
                fix: false,
                project: false,
            }
        }

        fn version(&self) -> &str {
            "0"
        }
    }

    /// A core build gets taplo, typos and astgrep from components and links only polyconfig,
    /// uncomment and quality for TOML; the merge must give the bundled build's order, with each
    /// cross-cutting component engine among the cross-cutting built-ins, not before polyconfig.
    #[test]
    fn external_engines_keep_the_bundled_order() {
        const TOML: &[Language] = &[Language::Toml];
        let built_in: Vec<Box<dyn crate::engine::Engine>> = vec![
            Box::new(Fake("polyconfig", TOML, false)),
            Box::new(Fake("uncomment", &[], false)),
            Box::new(Fake("quality", &[], false)),
        ];
        let external: Vec<Box<dyn crate::engine::Engine>> = vec![
            Box::new(Fake("taplo", TOML, true)),
            // Not the bundled order: the merge must not depend on the component index's order.
            Box::new(Fake("astgrep", &[], false)),
            Box::new(Fake("typos", &[], false)),
        ];
        let merged = super::merge_external_engines(built_in, external);
        let names: Vec<&str> = merged.iter().map(|engine| engine.name()).collect();
        assert_eq!(
            names,
            ["taplo", "polyconfig", "typos", "astgrep", "uncomment", "quality"]
        );

        // Several component engines for one language arrive in index (name) order; the bundled
        // order has the formatter first.
        const MARKDOWN: &[Language] = &[Language::Markdown];
        let external: Vec<Box<dyn crate::engine::Engine>> = vec![
            Box::new(Fake("adrlint", MARKDOWN, false)),
            Box::new(Fake("rumdl", MARKDOWN, true)),
        ];
        let merged = super::merge_external_engines(vec![Box::new(Fake("quality", &[], false))], external);
        let names: Vec<&str> = merged.iter().map(|engine| engine.name()).collect();
        assert_eq!(names, ["rumdl", "adrlint", "quality"]);
    }
}
