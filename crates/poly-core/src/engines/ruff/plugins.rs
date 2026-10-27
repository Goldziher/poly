//! Ruff plugin and fix-policy options read from `[lint.python.ruff]`.
//!
//! Ruff nests these under `[lint.<plugin>]`; poly keeps its options flat, so the
//! table name becomes a key prefix (`flake8_quotes_inline_quotes`,
//! `pep8_naming_extend_ignore_names`, `pycodestyle_max_line_length`). Values
//! keep ruff's own vocabulary.

use ruff_linter::line_width::LineLength;
use ruff_linter::registry::Rule;
use ruff_linter::rules::flake8_pytest_style::types::{
    ParametrizeNameType, ParametrizeValuesRowType, ParametrizeValuesType,
};
use ruff_linter::rules::flake8_quotes::settings::Quote;
use ruff_linter::rules::flake8_type_checking::settings::RuntimeSemantics;
use ruff_linter::rules::pep8_naming::settings::IgnoreNames;
use ruff_linter::settings::LinterSettings;

use crate::config::EngineConfig;

/// Apply the plugin options and the fix policy onto `settings`.
pub(super) fn apply(settings: &mut LinterSettings, cfg: &EngineConfig) {
    let options = Options { cfg };
    options.general(settings);
    options.pycodestyle(settings);
    options.pep8_naming(settings);
    options.flake8_quotes(settings);
    options.flake8_type_checking(settings);
    options.flake8_pytest_style(settings);
    options.fix_policy(settings);
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
        let allowed = fixable.map(|codes| super::rules_for_codes(&codes));
        let denied = super::rules_for_codes(&unfixable);
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
