//! `.pre-commit-config.yaml` importer: known hook ids map onto poly builtins,
//! `repo: local` hooks become command jobs, everything else is reported.

use std::path::{Path, PathBuf};

use poly_config::Stage;
use serde_json::Value;

use super::acc::{Body, BuiltinUse, HookAcc, JobSpec, Shape, ToolSettings};
use super::command::{EngineOption, RUFF_UNSAFE_FIXES, Target, shfmt_indent};
use super::filters::regex_to_globs;
use crate::migrate::importers::load_yaml;

const CONFIG_NAME: &str = ".pre-commit-config.yaml";

/// How a known hook id is realised in poly.
#[derive(Clone, Copy)]
enum Mapping {
    Builtin(Target, &'static [&'static str]),
    FileSafety(&'static str),
}

struct Known {
    id: &'static str,
    mapping: Mapping,
    /// Why the mapping is only an approximation; `None` when it is equivalent.
    approximation: Option<&'static str>,
    /// Arguments that do not change what poly does.
    benign_args: &'static [&'static str],
    /// The files the hook inherently applies to, used when the config sets no `files`.
    default_files: &'static [&'static str],
    /// The catalog tool the hook's remaining `args` configure, and the settings the
    /// hook's own entry point implies (buildifier's `-mode=fix -lint=fix`).
    configures: Option<Configures>,
}

#[derive(Clone, Copy)]
struct Configures {
    tool: &'static str,
    /// Flags the hook always passes, for the formatting role.
    format_extra_args: &'static [&'static str],
    /// The hook also reports what it cannot fix, i.e. it lints too.
    also_lint: bool,
}

impl Known {
    const fn scoped(mut self, default_files: &'static [&'static str]) -> Self {
        self.default_files = default_files;
        self
    }
}

const FORMATS_ONLY_KNOWN_LANGUAGES: &str =
    "poly only normalizes files of languages it formats; other file types are left as they are";

const KNOWN: &[Known] = &[
    known(
        "trailing-whitespace",
        Mapping::Builtin(Target::Fmt, &[]),
        Some(FORMATS_ONLY_KNOWN_LANGUAGES),
    ),
    known(
        "end-of-file-fixer",
        Mapping::Builtin(Target::Fmt, &[]),
        Some(FORMATS_ONLY_KNOWN_LANGUAGES),
    ),
    known(
        "mixed-line-ending",
        Mapping::Builtin(Target::Fmt, &[]),
        Some(FORMATS_ONLY_KNOWN_LANGUAGES),
    ),
    known("check-yaml", Mapping::Builtin(Target::Lint, &[]), None).scoped(&["*.{yml,yaml}"]),
    known("check-toml", Mapping::Builtin(Target::Lint, &[]), None).scoped(&["*.toml"]),
    known("check-json", Mapping::Builtin(Target::Lint, &[]), None).scoped(&["*.json"]),
    known("check-ast", Mapping::Builtin(Target::Lint, &[]), None).scoped(&["*.{py,pyi}"]),
    known(
        "check-added-large-files",
        Mapping::FileSafety("added_large_files"),
        None,
    ),
    known("check-merge-conflict", Mapping::FileSafety("merge_conflict"), None),
    known("detect-private-key", Mapping::FileSafety("private_key"), None),
    Known {
        benign_args: &[ALLOW_MISSING_CREDENTIALS],
        ..known("detect-aws-credentials", Mapping::FileSafety("aws_credentials"), None)
    },
    known("check-case-conflict", Mapping::FileSafety("case_conflict"), None),
    known(
        "check-executables-have-shebangs",
        Mapping::FileSafety("executables_have_shebangs"),
        None,
    ),
    known(
        "check-shebang-scripts-are-executable",
        Mapping::FileSafety("shebang_scripts_are_executable"),
        None,
    ),
    Known {
        benign_args: &["--fix", "--unsafe-fixes", "--exit-non-zero-on-fix", "--force-exclude"],
        ..known("ruff", Mapping::Builtin(Target::Lint, &[]), None).scoped(&["*.{py,pyi}"])
    },
    Known {
        benign_args: &["--fix", "--unsafe-fixes", "--exit-non-zero-on-fix", "--force-exclude"],
        ..known("ruff-check", Mapping::Builtin(Target::Lint, &[]), None).scoped(&["*.{py,pyi}"])
    },
    known("ruff-format", Mapping::Builtin(Target::Fmt, &[]), None).scoped(&["*.{py,pyi}"]),
    known(
        "black",
        Mapping::Builtin(Target::Fmt, &[]),
        Some("poly formats Python with ruff's formatter, which differs from black in edge cases"),
    )
    .scoped(&["*.{py,pyi}"]),
    known(
        "isort",
        Mapping::Builtin(Target::Lint, &[]),
        Some("import ordering is ruff's I001 rule under poly, with ruff's defaults rather than isort's"),
    )
    .scoped(&["*.{py,pyi}"]),
    known(
        "flake8",
        Mapping::Builtin(Target::Lint, &[]),
        Some("poly lints Python with ruff, whose rule set overlaps but is not flake8's"),
    )
    .scoped(&["*.{py,pyi}"]),
    known("typos", Mapping::Builtin(Target::Lint, &[]), None),
    known("shellcheck", Mapping::Builtin(Target::Lint, &[]), None).scoped(&["*.{sh,bash}"]),
    Known {
        benign_args: &["-w"],
        ..known("shfmt", Mapping::Builtin(Target::Fmt, &[]), None).scoped(&["*.{sh,bash}"])
    },
    Known {
        configures: Some(Configures {
            tool: "terraform",
            format_extra_args: &[],
            also_lint: false,
        }),
        ..known("terraform_fmt", Mapping::Builtin(Target::Fmt, &["terraform"]), None).scoped(&["*.{tf,tfvars}"])
    },
    known("taplo-format", Mapping::Builtin(Target::Fmt, &[]), None).scoped(&["*.toml"]),
    known("taplo-lint", Mapping::Builtin(Target::Lint, &[]), None).scoped(&["*.toml"]),
    Known {
        configures: Some(Configures {
            tool: "actionlint",
            format_extra_args: &[],
            also_lint: false,
        }),
        ..known("actionlint", Mapping::Builtin(Target::Lint, &["actionlint"]), None)
            .scoped(&[".github/workflows/*.{yml,yaml}"])
    },
    Known {
        configures: Some(Configures {
            tool: "buildifier",
            format_extra_args: &["-lint=fix"],
            also_lint: true,
        }),
        ..known("buildifier", Mapping::Builtin(Target::Fmt, &["buildifier"]), None).scoped(&[
            "*BUILD",
            "*BUILD.bazel",
            "*.bzl",
        ])
    },
    known(
        "golangci-lint",
        Mapping::Builtin(Target::Lint, &["golangci-lint"]),
        None,
    )
    .scoped(&["*.go"]),
    known(
        "markdownlint",
        Mapping::Builtin(Target::Lint, &[]),
        Some("poly lints Markdown with rumdl, not markdownlint"),
    ),
    known(
        "yamllint",
        Mapping::Builtin(Target::Lint, &[]),
        Some("poly lints YAML with its own engine, not yamllint"),
    ),
];

const fn known(id: &'static str, mapping: Mapping, approximation: Option<&'static str>) -> Known {
    Known {
        id,
        mapping,
        approximation,
        benign_args: &[],
        default_files: &[],
        configures: None,
    }
}

/// Hooks that rewrite the files they are given.
const FIXERS: &[&str] = &[
    "trailing-whitespace",
    "end-of-file-fixer",
    "mixed-line-ending",
    "ruff-format",
    "black",
    "isort",
    "terraform_fmt",
    "taplo-format",
    "buildifier",
    "shfmt",
];

/// Linters that only rewrite files when given `--fix`.
const FIXES_WITH_FLAG: &[&str] = &["ruff", "ruff-check"];

const NO_EQUIVALENT: &[(&str, &str)] = &[("requirements-txt-fixer", "poly does not sort requirements files")];

const ALLOW_MISSING_CREDENTIALS: &str = "--allow-missing-credentials";

/// What the pre-commit importer read.
#[derive(Debug)]
pub(super) struct Outcome {
    pub source: PathBuf,
    /// Whether every hook in the file now lives in `poly.toml` without loss.
    pub fully_absorbed: bool,
}

struct Context {
    default_stages: Vec<String>,
    global_exclude: Vec<String>,
}

/// Import `dir/.pre-commit-config.yaml`, if present.
pub(super) fn import(dir: &Path, acc: &mut HookAcc) -> Option<Outcome> {
    let source = dir.join(CONFIG_NAME);
    if !source.is_file() {
        return None;
    }
    let before = acc.diag.lossy_count();
    let Some(config) = load_yaml(&source) else {
        acc.diag.lossy(format!("{CONFIG_NAME}: unreadable or not valid YAML"));
        return Some(Outcome {
            source,
            fully_absorbed: false,
        });
    };
    let mut context = Context {
        default_stages: stage_names(config.get("default_stages")),
        global_exclude: Vec::new(),
    };
    if let Some(exclude) = config.get("exclude").and_then(Value::as_str) {
        context.global_exclude = globs(acc, "top-level `exclude`", exclude, true);
    }
    if config.get("files").is_some() {
        acc.diag.lossy(format!(
            "{CONFIG_NAME}: top-level `files` is not applied to individual hooks"
        ));
    }
    if config.get("fail_fast").and_then(Value::as_bool) == Some(true) {
        acc.diag
            .lossy(format!("{CONFIG_NAME}: fail_fast is not representable per run"));
    }
    for repo in config.get("repos").and_then(Value::as_array).into_iter().flatten() {
        let location = repo.get("repo").and_then(Value::as_str).unwrap_or("?");
        for hook in repo.get("hooks").and_then(Value::as_array).into_iter().flatten() {
            match location {
                "local" => local_hook(acc, &context, dir, hook),
                "meta" => acc.diag.note(format!("{CONFIG_NAME}: meta hook skipped")),
                _ => remote_hook(acc, &context, location, repo.get("rev").and_then(Value::as_str), hook),
            }
        }
    }
    Some(Outcome {
        source,
        fully_absorbed: acc.diag.lossy_count() == before,
    })
}

fn stage_names(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|name| name.parse::<Stage>().ok())
        .map(|stage| stage.as_str().to_string())
        .collect()
}

fn globs(acc: &mut HookAcc, label: &str, pattern: &str, quiet_empty: bool) -> Vec<String> {
    if pattern.trim().is_empty() && quiet_empty {
        return Vec::new();
    }
    regex_to_globs(pattern).unwrap_or_else(|reason| {
        acc.diag
            .lossy(format!("{label}: pattern not translated, filter dropped ({reason})"));
        Vec::new()
    })
}

fn hook_stages(context: &Context, hook: &Value) -> Vec<String> {
    let own = stage_names(hook.get("stages"));
    if !own.is_empty() {
        own
    } else if !context.default_stages.is_empty() {
        context.default_stages.clone()
    } else {
        vec!["pre-commit".to_string()]
    }
}

fn args_of(hook: &Value) -> Vec<String> {
    hook.get("args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn scope(acc: &mut HookAcc, context: &Context, label: &str, hook: &Value) -> (Vec<String>, Vec<String>) {
    let files = hook
        .get("files")
        .and_then(Value::as_str)
        .map(|p| globs(acc, &format!("{label} files"), p, true))
        .unwrap_or_default();
    let mut exclude = hook
        .get("exclude")
        .and_then(Value::as_str)
        .map(|p| globs(acc, &format!("{label} exclude"), p, true))
        .unwrap_or_default();
    for pattern in &context.global_exclude {
        if !exclude.contains(pattern) {
            exclude.push(pattern.clone());
        }
    }
    (files, exclude)
}

fn remote_hook(acc: &mut HookAcc, context: &Context, repo: &str, rev: Option<&str>, hook: &Value) {
    let Some(id) = hook.get("id").and_then(Value::as_str) else {
        return;
    };
    let label = format!("{CONFIG_NAME} {id}");
    let Some(entry) = KNOWN.iter().find(|k| k.id == id) else {
        let reason = NO_EQUIVALENT
            .iter()
            .find(|(known, _)| *known == id)
            .map_or("no poly equivalent is known for this hook", |(_, why)| *why);
        acc.diag.lossy(format!("{label} ({repo}): NOT MIGRATED - {reason}"));
        return;
    };
    let (mut files, exclude) = scope(acc, context, &label, hook);
    if files.is_empty() && hook.get("files").is_none() {
        files = entry.default_files.iter().map(|g| (*g).to_string()).collect();
    }
    let mut args = args_of(hook);
    if let Some(approximation) = entry.approximation {
        acc.diag.lossy(format!("{label}: approximated - {approximation}"));
    }
    for key in ["types_or", "exclude_types"] {
        if hook.get(key).is_some() {
            acc.diag.lossy(format!("{label}: `{key}` filter is not applied"));
        }
    }
    let fix = FIXERS.contains(&entry.id) || (FIXES_WITH_FLAG.contains(&entry.id) && args.iter().any(|a| a == "--fix"));
    let shape = Shape {
        files,
        exclude,
        fix,
        priority: 0,
    };
    let options: &[EngineOption] = if args.iter().any(|a| a == "--unsafe-fixes") {
        &[RUFF_UNSAFE_FIXES]
    } else {
        &[]
    };
    for stage in hook_stages(context, hook) {
        match entry.mapping {
            Mapping::Builtin(target, tools) => acc.add_builtin(BuiltinUse {
                target,
                stage: &stage,
                label: &label,
                shape: shape.clone(),
                catalog_tools: tools,
                options,
            }),
            Mapping::FileSafety(member) => acc.add_file_safety(member, &stage, &label, shape.clone()),
        }
    }
    if entry.id == "detect-aws-credentials" && !args.iter().any(|a| a == ALLOW_MISSING_CREDENTIALS) {
        acc.diag.lossy(format!(
            "{label}: approximated - pre-commit fails when no AWS credentials are configured; poly's check passes"
        ));
    }
    if entry.id == "check-added-large-files" {
        apply_maxkb(acc, &label, &mut args);
    }
    if entry.id == "shfmt" {
        apply_shfmt_indent(acc, &label, &mut args);
    }
    args.retain(|arg| !entry.benign_args.contains(&arg.as_str()));
    if let Some(configures) = entry.configures {
        configure_tool(acc, &label, configures, rev, std::mem::take(&mut args));
        if configures.also_lint {
            for stage in hook_stages(context, hook) {
                acc.add_builtin(BuiltinUse {
                    target: Target::Lint,
                    stage: &stage,
                    label: &label,
                    shape: shape.clone(),
                    catalog_tools: &[configures.tool],
                    options: &[],
                });
            }
        }
    }
    if !args.is_empty() {
        acc.diag.lossy(format!(
            "{label}: args {} have no poly equivalent and were dropped",
            args.join(" ")
        ));
    }
    acc.diag.note(format!("{label}: mapped to a poly builtin"));
}

/// Carry a hook's arguments and pinned `rev` to the catalog tool it runs.
fn configure_tool(acc: &mut HookAcc, label: &str, configures: Configures, rev: Option<&str>, args: Vec<String>) {
    let version = rev
        .map(|rev| rev.trim_start_matches('v').to_string())
        .filter(|rev| super::pin::catalog_has_release(configures.tool, rev));
    if version.is_none()
        && let Some(rev) = rev
        && super::pin::pin_file(configures.tool).is_none()
        && poly_catalog::Catalog::get()
            .tool(configures.tool)
            .and_then(|tool| tool.recipe.as_ref())
            .and_then(|recipe| recipe.install.as_ref())
            .is_some_and(|install| !install.releases.is_empty())
    {
        acc.diag.lossy(format!(
            "{label}: rev {rev} of {} is not a release poly can download; set `[tools.{}] version`, `url` and `sha256` to pin it",
            configures.tool, configures.tool
        ));
    }
    let settings = ToolSettings {
        extra_args: args,
        format_extra_args: configures.format_extra_args.iter().map(|a| (*a).to_string()).collect(),
        version,
        ..ToolSettings::default()
    };
    acc.add_tool_settings(configures.tool, label, &settings);
}

/// Carry shfmt's `-i <width>` into `[fmt.shell.shfmt]`; the rest of the arguments are left for the
/// caller to report.
fn apply_shfmt_indent(acc: &mut HookAcc, label: &str, args: &mut Vec<String>) {
    let mut kept = Vec::new();
    let mut iter = std::mem::take(args).into_iter();
    while let Some(arg) = iter.next() {
        let value = match arg.strip_prefix("-i=") {
            Some(value) => Some(value.to_string()),
            None if arg == "-i" => iter.next(),
            None => {
                kept.push(arg);
                continue;
            }
        };
        match value.as_deref().and_then(|v| v.parse::<i64>().ok()).filter(|&w| w >= 0) {
            Some(width) => {
                let (option, indent) = shfmt_indent(width);
                if let Some(option) = option {
                    acc.add_engine_option(&option);
                }
                if let Some(indent) = indent {
                    acc.add_engine_value(&indent, label);
                }
            }
            None => acc.diag.lossy(format!("{label}: unreadable -i value")),
        }
    }
    *args = kept;
}

fn apply_maxkb(acc: &mut HookAcc, label: &str, args: &mut Vec<String>) {
    let mut kept = Vec::new();
    let mut iter = std::mem::take(args).into_iter();
    while let Some(arg) = iter.next() {
        let value = match arg.strip_prefix("--maxkb=") {
            Some(value) => Some(value.to_string()),
            None if arg == "--maxkb" => iter.next(),
            None => {
                kept.push(arg);
                continue;
            }
        };
        match value.as_deref().and_then(|v| v.parse::<i64>().ok()) {
            Some(kb) => acc.set_max_added_file_kb(kb),
            None => acc.diag.lossy(format!("{label}: unreadable --maxkb value")),
        }
    }
    *args = kept;
}

fn local_hook(acc: &mut HookAcc, context: &Context, dir: &Path, hook: &Value) {
    let Some(id) = hook.get("id").and_then(Value::as_str) else {
        return;
    };
    let label = format!("{CONFIG_NAME} local {id}");
    let Some(entry) = hook.get("entry").and_then(Value::as_str) else {
        acc.diag.lossy(format!("{label}: NOT MIGRATED - no `entry`"));
        return;
    };
    let language = hook.get("language").and_then(Value::as_str).unwrap_or("system");
    if !matches!(language, "system" | "script" | "fail") {
        acc.diag.lossy(format!(
            "{label}: language `{language}` expects pre-commit to build an environment; poly runs the entry from PATH"
        ));
    }
    if hook.get("additional_dependencies").is_some() {
        acc.diag
            .lossy(format!("{label}: additional_dependencies are not installed by poly"));
    }
    for key in ["types_or", "exclude_types"] {
        if hook.get(key).is_some() {
            acc.diag.lossy(format!("{label}: `{key}` filter is not applied"));
        }
    }
    let (files, exclude) = scope(acc, context, &label, hook);
    let mut run = entry.trim().to_string();
    for arg in args_of(hook) {
        run.push(' ');
        run.push_str(&shell_word(&arg));
    }
    let script_path = entry.split_whitespace().next().filter(|p| dir.join(p).is_file());
    let mut job = JobSpec::new(
        id.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_', "_"),
        Body::Run(run),
    );
    if language == "script"
        && let Some(path) = script_path
    {
        let args = args_of(hook);
        job.body = Body::Script {
            path: path.to_string(),
            runner: None,
            args,
        };
    }
    job.files = if hook.get("always_run").and_then(Value::as_bool) == Some(true) {
        Vec::new()
    } else {
        files
    };
    job.exclude = exclude;
    job.workspace = hook.get("pass_filenames").and_then(Value::as_bool) == Some(false);
    job.file_types = hook
        .get("types")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    job.interactive = false;
    for stage in hook_stages(context, hook) {
        acc.add_job(&stage, job.clone());
    }
    acc.diag.note(format!("{label}: mapped to a command job"));
}

fn shell_word(arg: &str) -> String {
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,".contains(c)) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}
