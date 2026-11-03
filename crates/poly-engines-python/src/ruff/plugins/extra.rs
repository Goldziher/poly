//! The plugin tables and linter-wide settings beyond the ones [`super`] reads
//! first: every remaining `LinterSettings` field ruff accepts from its config.
//!
//! As elsewhere, ruff's `[lint.<plugin>]` table name becomes the key prefix
//! (`flake8_bandit_check_typed_exception`). Where ruff offers a replacing key and
//! an `extend-` key, the replacing key swaps the default and the `extend` one
//! adds to whichever list results.

use std::path::PathBuf;

use ruff_linter::rules::flake8_bandit::settings::default_tmp_dirs;
use ruff_linter::rules::flake8_gettext::settings::default_func_names;
use ruff_linter::rules::flake8_tidy_imports::settings::{ApiBan, ImportSelector, Strictness};
use ruff_linter::settings::LinterSettings;
use ruff_python_ast::name::Name;
use ruff_python_semantic::{Alias, MemberNameImport, ModuleNameImport, NameImport};
use rustc_hash::FxHashSet;

use super::{Options, choice};

impl Options<'_> {
    pub(super) fn extra_plugins(&self, settings: &mut LinterSettings) {
        self.linter_wide(settings);
        self.isort_required_imports(settings);
        self.flake8_annotations(settings);
        self.flake8_bandit(settings);
        self.flake8_call_lists_and_flags(settings);
        self.flake8_builtins(settings);
        self.flake8_copyright(settings);
        self.flake8_gettext(settings);
        self.flake8_self(settings);
        self.flake8_tidy_imports(settings);
        self.pydocstyle_extras(settings);
        self.pyflakes_pyupgrade_ruff(settings);
    }

    /// An import selector: `"all"`, a list of modules, or `{ include, exclude }`.
    fn import_selector(&self, key: &str) -> Option<ImportSelector> {
        from_value("import selector", self.cfg.options.get(key))
    }

    fn option_str(&self, key: &str) -> Option<&str> {
        self.cfg.options.get(key).and_then(toml::Value::as_str)
    }

    fn linter_wide(&self, settings: &mut LinterSettings) {
        if let Some(names) = self.str_list("builtins") {
            settings.builtins = names;
        }
        if let Some(names) = self.str_list("logger_objects") {
            settings.logger_objects = names;
        }
        if let Some(tags) = self.str_list("task_tags") {
            settings.task_tags = tags;
        }
        if let Some(modules) = self.str_list("typing_modules") {
            settings.typing_modules = modules;
        }
        if let Some(packages) = self.str_list("namespace_packages") {
            settings.namespace_packages = packages.into_iter().map(PathBuf::from).collect();
        }
        if let Some(characters) = self.str_list("allowed_confusables") {
            settings.allowed_confusables = characters
                .iter()
                .flat_map(|text| text.chars())
                .collect::<FxHashSet<char>>();
        }
        if let Some(value) = self.flag("typing_extensions") {
            settings.typing_extensions = value;
        }
        if let Some(value) = self.flag("future_annotations") {
            settings.future_annotations = value;
        }
        if let Some(value) = self.flag("ignore_init_module_imports") {
            settings.ignore_init_module_imports = value;
        }
    }

    /// `required-imports` lines such as `from __future__ import annotations`.
    fn isort_required_imports(&self, settings: &mut LinterSettings) {
        if let Some(lines) = self.str_list("isort_required_imports") {
            settings.isort.required_imports = lines.iter().filter_map(|line| required_import(line)).collect();
        }
    }

    fn flake8_annotations(&self, settings: &mut LinterSettings) {
        let annotations = &mut settings.flake8_annotations;
        if let Some(value) = self.flag("flake8_annotations_mypy_init_return") {
            annotations.mypy_init_return = value;
        }
        if let Some(value) = self.flag("flake8_annotations_suppress_dummy_args") {
            annotations.suppress_dummy_args = value;
        }
        if let Some(value) = self.flag("flake8_annotations_suppress_none_returning") {
            annotations.suppress_none_returning = value;
        }
        if let Some(value) = self.flag("flake8_annotations_allow_star_arg_any") {
            annotations.allow_star_arg_any = value;
        }
        if let Some(value) = self.flag("flake8_annotations_ignore_fully_untyped") {
            annotations.ignore_fully_untyped = value;
        }
    }

    fn flake8_bandit(&self, settings: &mut LinterSettings) {
        let bandit = &mut settings.flake8_bandit;
        let replaced = self.str_list("flake8_bandit_hardcoded_tmp_directory");
        let extended = self.str_list("flake8_bandit_hardcoded_tmp_directory_extend");
        if replaced.is_some() || extended.is_some() {
            bandit.hardcoded_tmp_directory = replaced
                .unwrap_or_else(default_tmp_dirs)
                .into_iter()
                .chain(extended.unwrap_or_default())
                .collect();
        }
        if let Some(value) = self.flag("flake8_bandit_check_typed_exception") {
            bandit.check_typed_exception = value;
        }
        if let Some(names) = self.str_list("flake8_bandit_extend_markup_names") {
            bandit.extend_markup_names = names;
        }
        if let Some(calls) = self.str_list("flake8_bandit_allowed_markup_calls") {
            bandit.allowed_markup_calls = calls;
        }
    }

    /// The plugins whose whole table is one or two settings.
    fn flake8_call_lists_and_flags(&self, settings: &mut LinterSettings) {
        if let Some(calls) = self.str_list("flake8_boolean_trap_extend_allowed_calls") {
            settings.flake8_boolean_trap.extend_allowed_calls = calls;
        }
        if let Some(calls) = self.str_list("flake8_bugbear_extend_immutable_calls") {
            settings.flake8_bugbear.extend_immutable_calls = calls;
        }
        if let Some(value) = self.flag("flake8_comprehensions_allow_dict_calls_with_keyword_arguments") {
            settings.flake8_comprehensions.allow_dict_calls_with_keyword_arguments = value;
        }
        if let Some(length) = self.usize_opt("flake8_errmsg_max_string_length") {
            settings.flake8_errmsg.max_string_length = length;
        }
        if let Some(value) = self.flag("flake8_implicit_str_concat_allow_multiline") {
            settings.flake8_implicit_str_concat.allow_multiline = value;
        }
        if let Some(value) = self.flag("flake8_unused_arguments_ignore_variadic_names") {
            settings.flake8_unused_arguments.ignore_variadic_names = value;
        }
    }

    fn flake8_builtins(&self, settings: &mut LinterSettings) {
        let builtins = &mut settings.flake8_builtins;
        if let Some(names) = self.str_list("flake8_builtins_ignorelist") {
            builtins.ignorelist = names;
        }
        if let Some(modules) = self.str_list("flake8_builtins_allowed_modules") {
            builtins.allowed_modules = modules;
        }
        if let Some(value) = self.flag("flake8_builtins_strict_checking") {
            builtins.strict_checking = value;
        }
    }

    fn flake8_copyright(&self, settings: &mut LinterSettings) {
        let copyright = &mut settings.flake8_copyright;
        if let Some(pattern) = self.option_str("flake8_copyright_notice_rgx") {
            match regex::Regex::new(pattern) {
                Ok(regex) => copyright.notice_rgx = regex,
                Err(error) => {
                    tracing::warn!(%error, engine = "ruff", "invalid flake8-copyright notice-rgx; keeping the default");
                }
            }
        }
        if let Some(author) = self.option_str("flake8_copyright_author") {
            copyright.author = Some(author.to_owned());
        }
        if let Some(size) = self.usize_opt("flake8_copyright_min_file_size") {
            copyright.min_file_size = size;
        }
    }

    fn flake8_gettext(&self, settings: &mut LinterSettings) {
        let replaced = self.str_list("flake8_gettext_function_names");
        let extended = self.str_list("flake8_gettext_extend_function_names");
        if replaced.is_some() || extended.is_some() {
            settings.flake8_gettext.function_names = replaced
                .map_or_else(default_func_names, |names| names.into_iter().map(Name::new).collect())
                .into_iter()
                .chain(extended.unwrap_or_default().into_iter().map(Name::new))
                .collect();
        }
    }

    fn flake8_self(&self, settings: &mut LinterSettings) {
        let replaced = self.str_list("flake8_self_ignore_names");
        let extended = self.str_list("flake8_self_extend_ignore_names");
        if replaced.is_some() || extended.is_some() {
            let base: Vec<Name> = replaced.map_or_else(
                || settings.flake8_self.ignore_names.clone(),
                |names| names.into_iter().map(Name::new).collect(),
            );
            settings.flake8_self.ignore_names = base
                .into_iter()
                .chain(extended.unwrap_or_default().into_iter().map(Name::new))
                .collect();
        }
    }

    /// `banned-api` replaces the banned members, `extend-banned-api` adds to them
    /// and wins where the two name the same member.
    fn flake8_tidy_imports(&self, settings: &mut LinterSettings) {
        let tidy = &mut settings.flake8_tidy_imports;
        let strictness = self.option_str("flake8_tidy_imports_ban_relative_imports");
        if let Some(strictness) = choice::<Strictness>("ban-relative-imports", strictness) {
            tidy.ban_relative_imports = strictness;
        }
        let banned = self
            .cfg
            .options
            .get("flake8_tidy_imports_banned_api")
            .and_then(toml::Value::as_table);
        let extended = self
            .cfg
            .options
            .get("flake8_tidy_imports_extend_banned_api")
            .and_then(toml::Value::as_table);
        if banned.is_some() || extended.is_some() {
            tidy.banned_api = banned
                .into_iter()
                .chain(extended)
                .flat_map(|table| table.iter())
                .filter_map(|(member, ban)| Some((member.clone(), from_value::<ApiBan>("banned-api", Some(ban))?)))
                .collect();
        }
        if let Some(modules) = self.str_list("flake8_tidy_imports_banned_module_level_imports") {
            tidy.banned_module_level_imports = modules;
        }
        if let Some(selector) = self.import_selector("flake8_tidy_imports_require_lazy") {
            tidy.require_lazy = selector;
        }
        if let Some(selector) = self.import_selector("flake8_tidy_imports_ban_lazy") {
            tidy.ban_lazy = selector;
        }
    }

    fn pydocstyle_extras(&self, settings: &mut LinterSettings) {
        let pydocstyle = &mut settings.pydocstyle;
        if let Some(decorators) = self.str_list("pydocstyle_ignore_decorators") {
            pydocstyle.ignore_decorators = decorators.into_iter().collect();
        }
        if let Some(decorators) = self.str_list("pydocstyle_property_decorators") {
            pydocstyle.property_decorators = decorators.into_iter().collect();
        }
        if let Some(value) = self.flag("pydocstyle_ignore_var_parameters") {
            pydocstyle.ignore_var_parameters = value;
        }
        if let Some(value) = self.flag("pydoclint_ignore_one_line_docstrings") {
            settings.pydoclint.ignore_one_line_docstrings = value;
        }
    }

    fn pyflakes_pyupgrade_ruff(&self, settings: &mut LinterSettings) {
        if let Some(generics) = self.str_list("pyflakes_extend_generics") {
            settings.pyflakes.extend_generics = generics;
        }
        if let Some(imports) = self.str_list("pyflakes_allowed_unused_imports") {
            settings.pyflakes.allowed_unused_imports = imports;
        }
        if let Some(value) = self.flag("pyupgrade_keep_runtime_typing") {
            settings.pyupgrade.keep_runtime_typing = value;
        }
        if let Some(value) = self.flag("ruff_parenthesize_tuple_in_subscript") {
            settings.ruff.parenthesize_tuple_in_subscript = value;
        }
        if let Some(value) = self.flag("ruff_strictly_empty_init_modules") {
            settings.ruff.strictly_empty_init_modules = value;
        }
    }
}

/// Deserialize `value` into `T`, warning and answering `None` when it does not fit.
fn from_value<T: serde::de::DeserializeOwned>(label: &str, value: Option<&toml::Value>) -> Option<T> {
    match value?.clone().try_into::<T>() {
        Ok(parsed) => Some(parsed),
        Err(error) => {
            tracing::warn!(%error, label, engine = "ruff", "unrecognised value; keeping the default");
            None
        }
    }
}

/// One `required-imports` entry: `import a`, `import a as b`, `from m import n`
/// or `from m import n as b`, with `m` possibly relative (`from . import n`).
fn required_import(line: &str) -> Option<NameImport> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let parsed = match words.as_slice() {
        ["import", name] => Some(NameImport::Import(ModuleNameImport::module((*name).to_owned()))),
        ["import", name, "as", alias] => Some(NameImport::Import(ModuleNameImport::alias(
            (*name).to_owned(),
            (*alias).to_owned(),
        ))),
        ["from", module, "import", member] => member_import(module, member, None),
        ["from", module, "import", member, "as", alias] => member_import(module, member, Some(alias)),
        _ => None,
    };
    if parsed.is_none() {
        tracing::warn!(line, engine = "ruff", "unrecognised isort required-import; ignoring it");
    }
    parsed
}

fn member_import(module: &str, member: &str, alias: Option<&str>) -> Option<NameImport> {
    let level = module.chars().take_while(|c| *c == '.').count();
    let rest = &module[level..];
    Some(NameImport::ImportFrom(MemberNameImport {
        module: (!rest.is_empty()).then(|| rest.to_owned()),
        name: Alias {
            name: member.to_owned(),
            as_name: alias.map(str::to_owned),
        },
        level: u32::try_from(level).ok()?,
    }))
}
