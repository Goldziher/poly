//! Ruff plugin and fix-policy options read from `[lint.python.ruff]`.
//!
//! Ruff nests these under `[lint.<plugin>]`; poly keeps its options flat, so the
//! table name becomes a key prefix (`flake8_quotes_inline_quotes`,
//! `pep8_naming_extend_ignore_names`, `pycodestyle_max_line_length`). Values
//! keep ruff's own vocabulary.

use ruff_linter::line_width::LineLength;
use ruff_linter::registry::Rule;
use ruff_linter::rule_selector::PreviewOptions;
use ruff_linter::rules::flake8_import_conventions::settings::BannedAliases;
use ruff_linter::rules::flake8_pytest_style::types::{
    ParametrizeNameType, ParametrizeValuesRowType, ParametrizeValuesType,
};
use ruff_linter::rules::flake8_quotes::settings::Quote;
use ruff_linter::rules::flake8_type_checking::settings::RuntimeSemantics;
use ruff_linter::rules::isort::categorize::{ImportSection, KnownModules};
use ruff_linter::rules::isort::settings::RelativeImportsOrder;
use ruff_linter::rules::pep8_naming::settings::IgnoreNames;
use ruff_linter::settings::LinterSettings;
use ruff_linter::settings::types::{IdentifierPattern, PreviewMode};
use rustc_hash::FxHashMap;

use crate::config::EngineConfig;

mod extra;

/// Apply the plugin options and the fix policy onto `settings`.
pub(super) fn apply(settings: &mut LinterSettings, cfg: &EngineConfig) {
    let options = Options { cfg };
    options.general(settings);
    options.pylint(settings);
    options.isort(settings);
    options.pycodestyle(settings);
    options.pep8_naming(settings);
    options.flake8_import_conventions(settings);
    options.flake8_quotes(settings);
    options.flake8_type_checking(settings);
    options.flake8_pytest_style(settings);
    options.extra_plugins(settings);
    options.fix_policy(settings);
}

/// The preview selection: `preview` turns preview rules on, and
/// `explicit_preview_rules` then requires them to be named by full code.
pub(super) fn preview_options(cfg: &EngineConfig) -> PreviewOptions {
    let flag = |key: &str| cfg.options.get(key).and_then(toml::Value::as_bool).unwrap_or(false);
    PreviewOptions {
        mode: PreviewMode::from(flag("preview")),
        require_explicit: flag("explicit_preview_rules"),
    }
}

struct Options<'a> {
    cfg: &'a EngineConfig,
}

impl Options<'_> {
    fn str_list(&self, key: &str) -> Option<Vec<String>> {
        self.cfg.options.get(key).and_then(toml::Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
    }

    fn flag(&self, key: &str) -> Option<bool> {
        self.cfg.options.get(key).and_then(toml::Value::as_bool)
    }

    fn usize_opt(&self, key: &str) -> Option<usize> {
        self.cfg
            .options
            .get(key)
            .and_then(toml::Value::as_integer)
            .and_then(|value| usize::try_from(value).ok())
    }

    fn general(&self, settings: &mut LinterSettings) {
        if let Some(external) = self.str_list("external") {
            settings.external = external;
        }
        if let Some(pattern) = self.cfg.options.get("dummy_variable_rgx").and_then(toml::Value::as_str) {
            match regex::Regex::new(pattern) {
                Ok(regex) => settings.dummy_variable_rgx = regex,
                Err(error) => {
                    tracing::warn!(%error, engine = "ruff", "invalid dummy_variable_rgx; keeping the default");
                }
            }
        }
    }

    fn pylint(&self, settings: &mut LinterSettings) {
        let pylint = &mut settings.pylint;
        if let Some(limit) = self.usize_opt("pylint_max_args") {
            pylint.max_args = limit;
        }
        if let Some(limit) = self.usize_opt("pylint_max_bool_expr") {
            pylint.max_bool_expr = limit;
        }
        if let Some(limit) = self.usize_opt("pylint_max_branches") {
            pylint.max_branches = limit;
        }
        if let Some(limit) = self.usize_opt("pylint_max_locals") {
            pylint.max_locals = limit;
        }
        if let Some(limit) = self.usize_opt("pylint_max_nested_blocks") {
            pylint.max_nested_blocks = limit;
        }
        if let Some(limit) = self.usize_opt("pylint_max_public_methods") {
            pylint.max_public_methods = limit;
        }
        if let Some(limit) = self.usize_opt("pylint_max_returns") {
            pylint.max_returns = limit;
        }
        if let Some(limit) = self.usize_opt("pylint_max_statements") {
            pylint.max_statements = limit;
        }
    }

    fn isort(&self, settings: &mut LinterSettings) {
        if let Some(src) = self.str_list("src").filter(|roots| !roots.is_empty()) {
            settings.src = src.iter().map(std::path::PathBuf::from).collect();
        }
        self.known_modules(settings);
        self.isort_flags(settings);
        self.isort_lists(settings);
        let isort = &mut settings.isort;
        let after = self
            .cfg
            .options
            .get("isort_lines_after_imports")
            .and_then(toml::Value::as_integer)
            .and_then(|lines| isize::try_from(lines).ok());
        if let Some(lines) = after {
            isort.lines_after_imports = lines;
        }
        if let Some(lines) = self.usize_opt("isort_lines_between_types") {
            isort.lines_between_types = lines;
        }
        let order = self
            .cfg
            .options
            .get("isort_relative_imports_order")
            .and_then(toml::Value::as_str);
        if let Some(order) = choice::<RelativeImportsOrder>("isort_relative_imports_order", order) {
            isort.relative_imports_order = order;
        }
        self.isort_sections(settings);
    }

    fn isort_flags(&self, settings: &mut LinterSettings) {
        let isort = &mut settings.isort;
        if let Some(value) = self.flag("isort_case_sensitive") {
            isort.case_sensitive = value;
        }
        if let Some(value) = self.flag("isort_combine_as_imports") {
            isort.combine_as_imports = value;
        }
        if let Some(value) = self.flag("isort_detect_same_package") {
            isort.detect_same_package = value;
        }
        if let Some(value) = self.flag("isort_force_single_line") {
            isort.force_single_line = value;
        }
        if let Some(value) = self.flag("isort_force_sort_within_sections") {
            isort.force_sort_within_sections = value;
        }
        if let Some(value) = self.flag("isort_force_wrap_aliases") {
            isort.force_wrap_aliases = value;
        }
        if let Some(value) = self.flag("isort_from_first") {
            isort.from_first = value;
        }
        if let Some(value) = self.flag("isort_length_sort") {
            isort.length_sort = value;
        }
        if let Some(value) = self.flag("isort_length_sort_straight") {
            isort.length_sort_straight = value;
        }
        if let Some(value) = self.flag("isort_no_sections") {
            isort.no_sections = value;
        }
        if let Some(value) = self.flag("isort_order_by_type") {
            isort.order_by_type = value;
        }
        if let Some(value) = self.flag("isort_split_on_trailing_comma") {
            isort.split_on_trailing_comma = value;
        }
    }

    fn isort_lists(&self, settings: &mut LinterSettings) {
        let isort = &mut settings.isort;
        if let Some(items) = self.str_list("isort_classes") {
            isort.classes = items.into_iter().collect();
        }
        if let Some(items) = self.str_list("isort_constants") {
            isort.constants = items.into_iter().collect();
        }
        if let Some(items) = self.str_list("isort_variables") {
            isort.variables = items.into_iter().collect();
        }
        if let Some(items) = self.str_list("isort_force_to_top") {
            isort.force_to_top = items.into_iter().collect();
        }
        if let Some(items) = self.str_list("isort_single_line_exclusions") {
            isort.single_line_exclusions = items.into_iter().collect();
        }
        if let Some(items) = self.str_list("isort_forced_separate") {
            isort.forced_separate = items;
        }
    }

    /// `section_order`, `default_section`, `no_lines_before` and `import_heading`
    /// all name sections: a built-in one (`future`, `standard-library`,
    /// `third-party`, `first-party`, `local-folder`) or one declared in
    /// `known_section`.
    fn isort_sections(&self, settings: &mut LinterSettings) {
        let section = |name: &String| choice::<ImportSection>("isort_section", Some(name));
        let isort = &mut settings.isort;
        if let Some(order) = self.str_list("isort_section_order") {
            isort.section_order = order.iter().filter_map(section).collect();
        }
        if let Some(name) = self
            .cfg
            .options
            .get("isort_default_section")
            .and_then(toml::Value::as_str)
            && let Some(default) = section(&name.to_owned())
        {
            isort.default_section = default;
        }
        if let Some(names) = self.str_list("isort_no_lines_before") {
            isort.no_lines_before = names.iter().filter_map(section).collect();
        }
        if let Some(headings) = self
            .cfg
            .options
            .get("isort_import_heading")
            .and_then(toml::Value::as_table)
        {
            isort.import_headings = headings
                .iter()
                .filter_map(|(name, text)| Some((section(name)?, text.as_str()?.to_owned())))
                .collect();
        }
    }

    /// The module classification: `known_first_party`, `known_third_party`,
    /// `known_local_folder`, `extra_standard_library` and the user-defined
    /// `known_section` table.
    fn known_modules(&self, settings: &mut LinterSettings) {
        let patterns = |list: Vec<String>| -> Vec<IdentifierPattern> {
            list.iter()
                .filter_map(|pattern| IdentifierPattern::new(pattern).ok())
                .collect()
        };
        let user_defined: FxHashMap<String, Vec<IdentifierPattern>> = self
            .cfg
            .options
            .get("known_section")
            .and_then(toml::Value::as_table)
            .map(|table| {
                table
                    .iter()
                    .map(|(section, modules)| {
                        let modules = modules
                            .as_array()
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(toml::Value::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default();
                        (section.clone(), patterns(modules))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (first, third, local, standard) = (
            patterns(self.str_list("known_first_party").unwrap_or_default()),
            patterns(self.str_list("known_third_party").unwrap_or_default()),
            patterns(self.str_list("known_local_folder").unwrap_or_default()),
            patterns(self.str_list("extra_standard_library").unwrap_or_default()),
        );
        if !(first.is_empty() && third.is_empty() && local.is_empty() && standard.is_empty() && user_defined.is_empty())
        {
            settings.isort.known_modules = KnownModules::new(first, third, local, standard, user_defined);
        }
    }

    fn pycodestyle(&self, settings: &mut LinterSettings) {
        if let Some(length) = line_length(self.usize_opt("pycodestyle_max_line_length")) {
            settings.pycodestyle.max_line_length = length;
        }
        if let Some(length) = line_length(self.usize_opt("pycodestyle_max_doc_length")) {
            settings.pycodestyle.max_doc_length = Some(length);
        }
        if let Some(ignore) = self.flag("pycodestyle_ignore_overlong_task_comments") {
            settings.pycodestyle.ignore_overlong_task_comments = ignore;
        }
    }

    fn pep8_naming(&self, settings: &mut LinterSettings) {
        let ignore = self.str_list("pep8_naming_ignore_names");
        let extend = self.str_list("pep8_naming_extend_ignore_names");
        if ignore.is_some() || extend.is_some() {
            match IgnoreNames::from_options(ignore, extend) {
                Ok(names) => settings.pep8_naming.ignore_names = names,
                Err(error) => {
                    tracing::warn!(%error, engine = "ruff", "invalid pep8-naming ignore names; keeping the default");
                }
            }
        }
        if let Some(decorators) = self.str_list("pep8_naming_classmethod_decorators") {
            settings.pep8_naming.classmethod_decorators = decorators;
        }
        if let Some(decorators) = self.str_list("pep8_naming_staticmethod_decorators") {
            settings.pep8_naming.staticmethod_decorators = decorators;
        }
    }

    fn flake8_quotes(&self, settings: &mut LinterSettings) {
        let options = &self.cfg.options;
        let quotes = &mut settings.flake8_quotes;
        let inline = options.get("flake8_quotes_inline_quotes").and_then(toml::Value::as_str);
        if let Some(quote) = quote("flake8_quotes_inline_quotes", inline) {
            quotes.inline_quotes = quote;
        }
        let multiline = options
            .get("flake8_quotes_multiline_quotes")
            .and_then(toml::Value::as_str);
        if let Some(quote) = quote("flake8_quotes_multiline_quotes", multiline) {
            quotes.multiline_quotes = quote;
        }
        let docstring = options
            .get("flake8_quotes_docstring_quotes")
            .and_then(toml::Value::as_str);
        if let Some(quote) = quote("flake8_quotes_docstring_quotes", docstring) {
            quotes.docstring_quotes = quote;
        }
        if let Some(avoid) = self.flag("flake8_quotes_avoid_escape") {
            quotes.avoid_escape = avoid;
        }
    }

    fn flake8_type_checking(&self, settings: &mut LinterSettings) {
        let checking = &mut settings.flake8_type_checking;
        if let Some(strict) = self.flag("flake8_type_checking_strict") {
            checking.strict = strict;
        }
        if let Some(quote) = self.flag("flake8_type_checking_quote_annotations") {
            checking.quote_annotations = quote;
        }
        if let Some(modules) = self.str_list("flake8_type_checking_exempt_modules") {
            checking.exempt_modules = modules;
        }
        if let Some(classes) = self.str_list("flake8_type_checking_runtime_evaluated_base_classes") {
            checking.runtime_evaluated_base_classes = required(classes);
        }
        if let Some(decorators) = self.str_list("flake8_type_checking_runtime_evaluated_decorators") {
            checking.runtime_evaluated_decorators = required(decorators);
        }
    }

    fn flake8_pytest_style(&self, settings: &mut LinterSettings) {
        let options = &self.cfg.options;
        let style = &mut settings.flake8_pytest_style;
        if let Some(parentheses) = self.flag("flake8_pytest_style_fixture_parentheses") {
            style.fixture_parentheses = parentheses;
        }
        if let Some(parentheses) = self.flag("flake8_pytest_style_mark_parentheses") {
            style.mark_parentheses = parentheses;
        }
        let names = options
            .get("flake8_pytest_style_parametrize_names_type")
            .and_then(toml::Value::as_str);
        if let Some(kind) = choice::<ParametrizeNameType>("flake8_pytest_style_parametrize_names_type", names) {
            style.parametrize_names_type = kind;
        }
        let values = options
            .get("flake8_pytest_style_parametrize_values_type")
            .and_then(toml::Value::as_str);
        if let Some(kind) = choice::<ParametrizeValuesType>("flake8_pytest_style_parametrize_values_type", values) {
            style.parametrize_values_type = kind;
        }
        let rows = options
            .get("flake8_pytest_style_parametrize_values_row_type")
            .and_then(toml::Value::as_str);
        if let Some(kind) = choice::<ParametrizeValuesRowType>("flake8_pytest_style_parametrize_values_row_type", rows)
        {
            style.parametrize_values_row_type = kind;
        }
    }

    /// `fixable` narrows which enabled rules may carry a fix; `unfixable` removes
    /// rules from that set. A rule that loses its fix still reports.
    fn fix_policy(&self, settings: &mut LinterSettings) {
        let fixable = self.str_list("fixable").filter(|codes| !is_all(codes));
        let unfixable = self.str_list("unfixable").unwrap_or_default();
        if fixable.is_none() && unfixable.is_empty() {
            return;
        }
        let preview = preview_options(self.cfg);
        let allowed = fixable.map(|codes| super::rules_for_codes(&codes, &preview));
        let denied = super::rules_for_codes(&unfixable, &preview);
        let enabled: Vec<Rule> = settings.rules.iter_enabled().collect();
        for rule in enabled {
            let may_fix = allowed.as_ref().is_none_or(|rules| rules.contains(&rule)) && !denied.contains(&rule);
            if !may_fix {
                settings.rules.disable(rule);
                settings.rules.enable(rule, false);
            }
        }
    }
}

impl Options<'_> {
    /// `aliases` replaces the conventional alias table, `extend_aliases` adds to
    /// it, `banned_aliases` maps a module to the aliases it must not use, and
    /// `banned_from` lists modules whose `from` imports are rejected.
    fn flake8_import_conventions(&self, settings: &mut LinterSettings) {
        let options = &self.cfg.options;
        let conventions = &mut settings.flake8_import_conventions;
        if let Some(aliases) = alias_pairs(
            options
                .get("flake8_import_conventions_aliases")
                .and_then(toml::Value::as_table),
        ) {
            conventions.aliases = aliases.into_iter().collect();
        }
        if let Some(extra) = alias_pairs(
            options
                .get("flake8_import_conventions_extend_aliases")
                .and_then(toml::Value::as_table),
        ) {
            conventions.aliases.extend(extra);
        }
        if let Some(banned) = options
            .get("flake8_import_conventions_banned_aliases")
            .and_then(toml::Value::as_table)
        {
            conventions.banned_aliases = banned
                .iter()
                .filter_map(|(module, aliases)| {
                    let aliases = aliases.as_array()?.iter().filter_map(toml::Value::as_str);
                    Some((module.clone(), aliases.map(str::to_owned).collect::<BannedAliases>()))
                })
                .collect();
        }
        if let Some(modules) = self.str_list("flake8_import_conventions_banned_from") {
            conventions.banned_from = modules.into_iter().collect();
        }
    }
}

/// A table of `module = "alias"` pairs; entries whose value is not a string are
/// skipped.
fn alias_pairs(table: Option<&toml::Table>) -> Option<Vec<(String, String)>> {
    table.map(|table| {
        table
            .iter()
            .filter_map(|(module, alias)| Some((module.clone(), alias.as_str()?.to_owned())))
            .collect()
    })
}

fn line_length(value: Option<usize>) -> Option<LineLength> {
    LineLength::try_from(u16::try_from(value?).ok()?).ok()
}

fn is_all(codes: &[String]) -> bool {
    codes.iter().any(|code| code.eq_ignore_ascii_case("ALL"))
}

fn required(names: Vec<String>) -> rustc_hash::FxHashMap<String, RuntimeSemantics> {
    names
        .into_iter()
        .map(|name| (name, RuntimeSemantics::Required))
        .collect()
}

fn quote(key: &str, raw: Option<&str>) -> Option<Quote> {
    match raw? {
        "double" => Some(Quote::Double),
        "single" => Some(Quote::Single),
        other => {
            tracing::warn!(key, value = other, engine = "ruff", "expected `single` or `double`");
            None
        }
    }
}

fn choice<T: serde::de::DeserializeOwned>(key: &str, raw: Option<&str>) -> Option<T> {
    let raw = raw?;
    let parsed = toml::Value::String(raw.to_owned()).try_into::<T>().ok();
    if parsed.is_none() {
        tracing::warn!(
            key,
            value = raw,
            engine = "ruff",
            "unrecognised value; keeping the default"
        );
    }
    parsed
}
