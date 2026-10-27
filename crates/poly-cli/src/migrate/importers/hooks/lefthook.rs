//! Lefthook importer: reads `lefthook.yml` (and everything it `extends`),
//! flattens the job tree, and feeds the shared [`HookAcc`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use poly_config::Stage;
use serde_json::{Map, Value};

use super::acc::{Body, Guard, HookAcc, JobSpec};
use super::command::{self, Recognition, Recognized, Target};
use super::filters::filter_to_globs;
use super::tree::{self, Mode, Node};
use crate::migrate::importers::{load_json, load_yaml};

const CONFIG_NAMES: &[&str] = &[
    "lefthook.yml",
    "lefthook.yaml",
    "lefthook.json",
    ".lefthook.yml",
    ".lefthook.yaml",
    ".lefthook.json",
    ".config/lefthook.yml",
    ".config/lefthook.yaml",
    ".config/lefthook.json",
];

const LOCAL_CONFIG_NAMES: &[&str] = &["lefthook-local.yml", "lefthook-local.yaml", ".lefthook-local.yml"];

const MAX_EXTENDS_DEPTH: usize = 8;

/// Top-level lefthook keys that configure the tool itself rather than a hook.
const GLOBAL_KEYS: &[&str] = &[
    "min_version",
    "colors",
    "no_tty",
    "output",
    "skip_output",
    "source_dir",
    "source_dir_local",
    "extends",
    "rc",
    "lefthook",
    "assert_lefthook_installed",
    "templates",
];

const JOB_KEYS: &[&str] = &[
    "name",
    "run",
    "script",
    "runner",
    "args",
    "glob",
    "exclude",
    "root",
    "env",
    "tags",
    "skip",
    "only",
    "stage_fixed",
    "fail_text",
    "interactive",
    "use_stdin",
    "files",
    "file_types",
    "priority",
    "group",
];

const SCRIPT_RUNNERS: &[&str] = &["bash", "sh", "zsh", "python", "python3", "node"];

/// A `prek` / `pre-commit` invocation held back until the pre-commit config has
/// been analysed.
#[derive(Debug)]
pub(super) struct PreCommitJob {
    pub stage: String,
    pub label: String,
    pub job: JobSpec,
}

/// What the lefthook importer read and deferred.
#[derive(Debug, Default)]
pub(super) struct Outcome {
    pub sources: Vec<PathBuf>,
    pub pre_commit_jobs: Vec<PreCommitJob>,
}

/// Import the lefthook configuration in `dir`, if any.
pub(super) fn import(dir: &Path, acc: &mut HookAcc) -> Option<Outcome> {
    let main = CONFIG_NAMES.iter().map(|n| dir.join(n)).find(|p| p.is_file())?;
    let mut outcome = Outcome::default();
    let merged = resolve(dir, &main, 0, &mut outcome.sources, acc)?;
    for name in LOCAL_CONFIG_NAMES {
        if dir.join(name).is_file() {
            acc.diag
                .lossy(format!("{name}: per-developer lefthook overrides are not read"));
        }
    }
    let mut importer = Importer {
        dir,
        acc,
        source_dir: merged
            .get("source_dir")
            .and_then(Value::as_str)
            .unwrap_or(".lefthook")
            .to_string(),
        pre_commit_jobs: Vec::new(),
        rewriting_builtins: Vec::new(),
        unordered_builtins: Vec::new(),
        inline_jobs: 0,
    };
    importer.run(&merged);
    outcome.pre_commit_jobs = importer.pre_commit_jobs;
    Some(outcome)
}

fn load_value(path: &Path) -> Option<Value> {
    if path.extension().is_some_and(|e| e == "json") {
        load_json(path)
    } else {
        load_yaml(path)
    }
}

fn resolve(dir: &Path, path: &Path, depth: usize, sources: &mut Vec<PathBuf>, acc: &mut HookAcc) -> Option<Value> {
    let shown = path.strip_prefix(dir).unwrap_or(path).display().to_string();
    let Some(Value::Object(mut config)) = load_value(path) else {
        acc.diag.lossy(format!("{shown}: unreadable or not a mapping"));
        return None;
    };
    sources.push(path.to_path_buf());
    let mut base = Value::Object(Map::new());
    let extends = config.remove("extends");
    if let Some(extends) = extends {
        for target in strings(&extends) {
            if depth >= MAX_EXTENDS_DEPTH {
                acc.diag
                    .lossy(format!("{shown}: extends nested deeper than {MAX_EXTENDS_DEPTH}"));
            } else if target.contains(['*', '?', '[']) {
                acc.diag
                    .lossy(format!("{shown}: extends glob `{target}` is not expanded"));
            } else {
                let candidate = dir.join(&target);
                if !candidate.is_file() {
                    acc.diag.lossy(format!("{shown}: extends `{target}` not found"));
                } else if !sources.contains(&candidate)
                    && let Some(extended) = resolve(dir, &candidate, depth + 1, sources, acc)
                {
                    base = merge(base, extended);
                }
            }
        }
    }
    Some(merge(base, Value::Object(config)))
}

/// Deep-merge `over` onto `base` the way lefthook merges `extends`: mappings
/// merge key by key, `jobs` lists merge by job name, anything else is replaced.
fn merge(base: Value, over: Value) -> Value {
    match (base, over) {
        (Value::Object(mut base), Value::Object(over)) => {
            for (key, value) in over {
                let merged = match base.remove(&key) {
                    Some(existing) if key == "jobs" => merge_jobs(existing, value),
                    Some(existing) => merge(existing, value),
                    None => value,
                };
                base.insert(key, merged);
            }
            Value::Object(base)
        }
        (_, over) => over,
    }
}

fn merge_jobs(base: Value, over: Value) -> Value {
    let (Value::Array(mut jobs), Value::Array(extra)) = (base, over) else {
        return Value::Array(Vec::new());
    };
    for job in extra {
        let name = job.get("name").and_then(Value::as_str).map(str::to_string);
        let existing = name.as_deref().and_then(|name| {
            jobs.iter()
                .position(|j| j.get("name").and_then(Value::as_str) == Some(name))
        });
        match existing {
            Some(index) => {
                let current = jobs.remove(index);
                jobs.insert(index, merge(current, job));
            }
            None => jobs.push(job),
        }
    }
    Value::Array(jobs)
}

fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        _ => Vec::new(),
    }
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.as_str(), "true" | "always"),
        _ => false,
    }
}

/// Settings a group passes down to the jobs inside it.
#[derive(Debug, Clone, Default)]
struct Inherit {
    root: Option<String>,
    env: BTreeMap<String, String>,
    glob: Vec<String>,
    exclude: Vec<String>,
    tags: Vec<String>,
    skip: Option<Guard>,
    only: Option<Guard>,
}

struct Leaf {
    job: Map<String, Value>,
    inherit: Inherit,
    name: Option<String>,
}

struct Importer<'a> {
    dir: &'a Path,
    acc: &'a mut HookAcc,
    source_dir: String,
    pre_commit_jobs: Vec<PreCommitJob>,
    rewriting_builtins: Vec<String>,
    unordered_builtins: Vec<String>,
    inline_jobs: usize,
}

impl Importer<'_> {
    fn run(&mut self, config: &Value) {
        let Some(map) = config.as_object() else { return };
        let mut ignored = Vec::new();
        for (key, value) in map {
            if GLOBAL_KEYS.contains(&key.as_str()) {
                if matches!(key.as_str(), "output" | "colors" | "skip_output" | "no_tty") {
                    ignored.push(key.as_str());
                }
                continue;
            }
            if key == "remotes" {
                self.acc
                    .diag
                    .lossy("lefthook remotes: remote configs are not fetched or imported");
                continue;
            }
            match key.parse::<Stage>() {
                Ok(stage) => self.stage(stage.as_str(), value),
                Err(_) => self.acc.diag.lossy(format!("lefthook: unknown top-level key `{key}`")),
            }
        }
        if !ignored.is_empty() {
            self.acc.diag.note(format!(
                "lefthook output settings have no poly counterpart: {}",
                ignored.join(", ")
            ));
        }
        self.summary_notes();
    }

    fn summary_notes(&mut self) {
        if !self.rewriting_builtins.is_empty() {
            self.acc.diag.note(format!(
                "behavior change: the fmt/lint builtins only check (`poly fmt --check`, `poly lint`); these jobs \
                 rewrote files and re-staged them: {}",
                self.rewriting_builtins.join(", ")
            ));
        }
        if !self.unordered_builtins.is_empty() {
            self.acc.diag.note(format!(
                "builtins run at the default priority (0); lefthook's ordering is not reproduced for: {}",
                self.unordered_builtins.join(", ")
            ));
        }
        if self.inline_jobs > 0 {
            self.acc.diag.note(
                "lefthook ran jobs on the working tree; poly's pre-commit gate validates the staged snapshot, \
                 so jobs that read untracked or ignored files (node_modules, build output) need `[hooks] isolate = false`"
                    .to_string(),
            );
        }
    }

    fn stage(&mut self, stage: &str, value: &Value) {
        let Some(config) = value.as_object() else { return };
        let mode = if truthy(config.get("piped")) {
            Mode::Piped
        } else if truthy(config.get("parallel")) {
            Mode::Parallel
        } else {
            Mode::Sequential
        };
        let root = Inherit::default();
        let mut children = Vec::new();
        if let Some(Value::Array(jobs)) = config.get("jobs") {
            children.extend(jobs.iter().filter_map(|j| self.node(stage, j, &root)));
        }
        children.extend(self.legacy_children(config.get("commands"), false));
        children.extend(self.legacy_children(config.get("scripts"), true));

        self.stage_settings(stage, config, mode);
        let placed = tree::layout(Node::Group { mode, children });
        let deepest = placed.iter().map(|p| p.layer).max().unwrap_or(0);
        let mut per_layer: BTreeMap<i64, usize> = BTreeMap::new();
        for entry in &placed {
            *per_layer.entry(entry.layer).or_default() += 1;
        }
        if per_layer.values().any(|count| *count > 1) {
            self.acc.stage(stage).parallel = true;
        }
        for entry in placed {
            self.leaf(stage, entry.leaf, entry.layer, deepest);
        }
    }

    fn stage_settings(&mut self, stage: &str, config: &Map<String, Value>, mode: Mode) {
        let label = stage.to_string();
        let known = [
            "jobs",
            "commands",
            "scripts",
            "parallel",
            "piped",
            "follow",
            "skip",
            "only",
            "exclude_tags",
            "fail_on_changes",
            "files",
            "glob",
            "exclude",
            "root",
            "env",
            "tags",
        ];
        for key in config.keys().filter(|k| !known.contains(&k.as_str())) {
            self.acc.diag.lossy(format!("{label}: unsupported stage key `{key}`"));
        }
        for key in ["files", "glob", "exclude", "root", "env", "tags"] {
            if config.contains_key(key) {
                self.acc
                    .diag
                    .lossy(format!("{label}: stage-level `{key}` is not applied to its jobs"));
            }
        }
        let skip = self.guard(&label, "skip", config.get("skip"));
        let only = self.guard(&label, "only", config.get("only"));
        let exclude_tags = config.get("exclude_tags").map(strings).unwrap_or_default();
        let stage_acc = self.acc.stage(stage);
        stage_acc.piped = mode == Mode::Piped;
        stage_acc.follow = truthy(config.get("follow"));
        stage_acc.fail_on_changes = truthy(config.get("fail_on_changes"));
        stage_acc.exclude_tags = exclude_tags;
        stage_acc.skip = skip;
        stage_acc.only = only;
        if let Some(value) = config.get("fail_on_changes")
            && !matches!(value.as_str(), Some("true" | "always" | "false" | "never"))
            && !value.is_boolean()
        {
            self.acc.diag.lossy(format!(
                "{label}: fail_on_changes = {value} is conditional; poly's is unconditional and was not set"
            ));
        }
    }

    fn legacy_children(&mut self, value: Option<&Value>, scripts: bool) -> Vec<Node<Leaf>> {
        let Some(Value::Object(entries)) = value else {
            return Vec::new();
        };
        let mut ordered: Vec<(&String, &Map<String, Value>)> = entries
            .iter()
            .filter_map(|(name, job)| job.as_object().map(|map| (name, map)))
            .collect();
        ordered.sort_by_key(|(name, job)| {
            (
                job.get("priority").and_then(Value::as_i64).unwrap_or(0),
                (*name).clone(),
            )
        });
        let root = Inherit::default();
        ordered
            .into_iter()
            .map(|(name, job)| {
                let mut job = job.clone();
                if scripts {
                    job.insert("script".to_string(), Value::String(name.clone()));
                }
                Node::Leaf(Leaf {
                    job,
                    inherit: root.clone(),
                    name: Some(name.clone()),
                })
            })
            .collect()
    }

    fn node(&mut self, stage: &str, value: &Value, parent: &Inherit) -> Option<Node<Leaf>> {
        let job = value.as_object()?;
        let name = job.get("name").and_then(Value::as_str).map(str::to_string);
        let label = format!("{stage}/{}", name.as_deref().unwrap_or("?"));
        let inherit = self.inherit(&label, job, parent);
        let Some(group) = job.get("group").and_then(Value::as_object) else {
            return Some(Node::Leaf(Leaf {
                job: job.clone(),
                inherit,
                name,
            }));
        };
        let mode = if truthy(group.get("piped")) {
            Mode::Piped
        } else if truthy(group.get("parallel")) {
            Mode::Parallel
        } else {
            Mode::Sequential
        };
        let inherit = self.inherit(&label, group, &inherit);
        let children = group
            .get("jobs")
            .and_then(Value::as_array)
            .map(|jobs| jobs.iter().filter_map(|j| self.node(stage, j, &inherit)).collect())
            .unwrap_or_default();
        Some(Node::Group { mode, children })
    }

    fn inherit(&mut self, label: &str, job: &Map<String, Value>, parent: &Inherit) -> Inherit {
        let mut inherit = parent.clone();
        if let Some(root) = job.get("root").and_then(Value::as_str) {
            inherit.root = Some(root.to_string());
        }
        if let Some(Value::Object(env)) = job.get("env") {
            for (key, value) in env {
                inherit.env.insert(key.clone(), scalar(value));
            }
        }
        if let Some(tags) = job.get("tags") {
            inherit.tags.extend(tags_of(tags));
        }
        if job.contains_key("group") {
            if let Some(glob) = job.get("glob") {
                inherit.glob = strings(glob);
            }
            if let Some(exclude) = job.get("exclude") {
                let (globs, _) = self.exclude_globs(label, exclude);
                inherit.exclude.extend(globs);
            }
            if let Some(guard) = self.guard(label, "skip", job.get("skip")) {
                inherit.skip = Some(guard);
            }
            if let Some(guard) = self.guard(label, "only", job.get("only")) {
                inherit.only = Some(guard);
            }
        }
        inherit
    }

    fn guard(&mut self, label: &str, key: &str, value: Option<&Value>) -> Option<Guard> {
        let value = value?;
        if let Value::Bool(flag) = value {
            return Some(Guard::Always(*flag));
        }
        let mut runs = Vec::new();
        let mut unsupported = Vec::new();
        for item in value.as_array().cloned().unwrap_or_else(|| vec![value.clone()]) {
            match item.get("run").and_then(Value::as_str) {
                Some(run) if item.get("ref").is_none() => runs.push(run.to_string()),
                _ => unsupported.push(match &item {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                }),
            }
        }
        if !unsupported.is_empty() {
            self.acc.diag.lossy(format!(
                "{label}: `{key}` condition(s) {} are not evaluated by poly (only `run` conditions are); \
                 the job runs where lefthook would have {}",
                unsupported.join(", "),
                if key == "skip" { "skipped it" } else { "restricted it" }
            ));
        }
        (!runs.is_empty()).then_some(Guard::Runs(runs))
    }

    fn exclude_globs(&mut self, label: &str, value: &Value) -> (Vec<String>, bool) {
        let mut globs = Vec::new();
        let mut complete = true;
        for pattern in strings(value) {
            match filter_to_globs(&pattern) {
                Ok(found) => globs.extend(found),
                Err(reason) => {
                    complete = false;
                    self.acc
                        .diag
                        .lossy(format!("{label}: exclude `{pattern}` not translated ({reason})"));
                }
            }
        }
        (globs, complete)
    }

    fn leaf(&mut self, stage: &str, leaf: Leaf, layer: i64, deepest: i64) {
        let Leaf { job, inherit, name } = leaf;
        let name = name.or_else(|| derived_name(&job)).unwrap_or_else(|| "job".to_string());
        let label = format!("{stage}/{name}");
        for key in job.keys().filter(|k| !JOB_KEYS.contains(&k.as_str())) {
            self.acc.diag.lossy(format!("{label}: unsupported job key `{key}`"));
        }
        if job.contains_key("files") {
            self.acc.diag.lossy(format!(
                "{label}: custom `files` command is not translated; staged files are used"
            ));
        }
        if job.contains_key("args") && !job.contains_key("script") {
            self.acc
                .diag
                .lossy(format!("{label}: `args` without `script` is ignored"));
        }

        let mut files = inherit.glob.clone();
        if let Some(glob) = job.get("glob") {
            files = strings(glob);
        }
        let mut exclude = inherit.exclude.clone();
        if let Some(value) = job.get("exclude") {
            let (globs, _) = self.exclude_globs(&label, value);
            exclude.extend(globs);
        }
        let skip = self.guard(&label, "skip", job.get("skip")).or(inherit.skip.clone());
        let only = self.guard(&label, "only", job.get("only")).or(inherit.only.clone());

        let mut spec = match self.body(stage, &label, &name, &job) {
            Resolved::Skip => return,
            Resolved::Builtin(recognized) => {
                let scope = Scope {
                    stage,
                    label: &label,
                    files: &files,
                    exclude: &exclude,
                    layer,
                    deepest,
                };
                self.builtin(&scope, &recognized, &job, &inherit);
                return;
            }
            Resolved::Job(spec) => *spec,
        };
        spec.files = files;
        spec.exclude = exclude;
        spec.skip = skip;
        spec.only = only;
        spec.priority = layer;
        spec.stage_fixed = truthy(job.get("stage_fixed"));
        spec.interactive = truthy(job.get("interactive"));
        spec.use_stdin = truthy(job.get("use_stdin"));
        spec.root = job
            .get("root")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(inherit.root.clone());
        spec.env = inherit.env.clone();
        if let Some(Value::Object(env)) = job.get("env") {
            spec.env.extend(env.iter().map(|(k, v)| (k.clone(), scalar(v))));
        }
        spec.tags = inherit.tags.clone();
        if let Some(tags) = job.get("tags") {
            spec.tags.extend(tags_of(tags));
        }
        spec.fail_text = job.get("fail_text").and_then(Value::as_str).map(str::to_string);
        spec.file_types = job.get("file_types").map(strings).unwrap_or_default();
        if is_prek(&spec) {
            self.pre_commit_jobs.push(PreCommitJob {
                stage: stage.to_string(),
                label,
                job: spec,
            });
        } else {
            self.inline_jobs += 1;
            self.acc.add_job(stage, spec);
        }
    }

    fn builtin(&mut self, scope: &Scope, recognized: &Recognized, job: &Map<String, Value>, inherit: &Inherit) {
        let label = scope.label;
        self.acc.add_builtin(
            recognized.target,
            scope.stage,
            label,
            scope.files,
            scope.exclude,
            recognized.catalog_tools,
        );
        let tool = match recognized.target {
            Target::Fmt => "fmt",
            Target::Lint => "lint",
        };
        let run = job.get("run").and_then(Value::as_str).unwrap_or_default();
        self.acc
            .diag
            .note(format!("{label}: `{}` -> [hooks.builtin] {tool}", run.trim()));
        for flag in &recognized.dropped_flags {
            self.acc.diag.lossy(format!(
                "{label}: {flag} has no poly equivalent; the lint builtin applies no fixes"
            ));
        }
        let rewrites = truthy(job.get("stage_fixed")) || run.split_whitespace().any(|t| t == "--fix");
        if rewrites {
            self.rewriting_builtins.push(label.to_string());
        }
        if scope.deepest > 0 {
            self.unordered_builtins.push(format!("{label} (layer {})", scope.layer));
        }
        let guarded = ["skip", "only"].iter().any(|key| job.contains_key(*key))
            || inherit.skip.is_some()
            || inherit.only.is_some();
        if guarded {
            self.acc.diag.lossy(format!(
                "{label}: skip/only guard cannot be attached to a builtin hook and was dropped"
            ));
        }
        if job.contains_key("root") || inherit.root.is_some() {
            self.acc.diag.lossy(format!(
                "{label}: `root` cannot be attached to a builtin hook and was dropped"
            ));
        }
    }

    fn body(&mut self, stage: &str, label: &str, name: &str, job: &Map<String, Value>) -> Resolved {
        if let Some(script) = job.get("script").and_then(Value::as_str) {
            let path = format!("{}/{stage}/{script}", self.source_dir);
            if !self.dir.join(&path).is_file() {
                self.acc.diag.lossy(format!("{label}: script `{path}` not found"));
            }
            let runner = job.get("runner").and_then(Value::as_str).map(str::to_string);
            let args = job.get("args").map(strings).unwrap_or_default();
            let mut spec = JobSpec::new(sanitize(name), Body::Script { path, runner, args });
            spec.workspace = true;
            return Resolved::Job(Box::new(spec));
        }
        let Some(run) = job.get("run").and_then(Value::as_str) else {
            self.acc
                .diag
                .lossy(format!("{label}: job has neither `run` nor `script`; not migrated"));
            return Resolved::Skip;
        };
        let missing = command::unsupported_placeholders(run);
        if !missing.is_empty() {
            self.acc.diag.lossy(format!(
                "{label}: NOT MIGRATED - `{}` uses {} which poly does not substitute",
                run.trim(),
                missing
                    .iter()
                    .map(|m| format!("{{{m}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return Resolved::Skip;
        }
        match command::recognize(run) {
            Recognition::Builtin(recognized) => return Resolved::Builtin(recognized),
            Recognition::FlagsNotRepresentable { tool, flags } => self.acc.diag.note(format!(
                "{label}: kept as a command - {tool} flags {} have no builtin equivalent",
                flags.join(" ")
            )),
            Recognition::NoEquivalent(reason) => self.acc.diag.note(format!("{label}: kept as a command - {reason}")),
            Recognition::Unknown => {}
        }
        Resolved::Job(Box::new(self.command_job(name, run)))
    }

    fn command_job(&self, name: &str, run: &str) -> JobSpec {
        let tokens = command::tokenize(run);
        let has_files = tokens.iter().any(|t| command::is_file_list(t));
        let mut spec = JobSpec::new(sanitize(name), Body::Run(run.trim().to_string()));
        spec.workspace = !has_files;
        if let Some(script) = self.script_form(&tokens) {
            spec.body = script;
        }
        spec
    }

    /// `bash path/to/script.sh args... {staged_files}` as a poly script job.
    fn script_form(&self, tokens: &[String]) -> Option<Body> {
        let runner = tokens.first()?;
        let path = tokens.get(1)?;
        if !SCRIPT_RUNNERS.contains(&runner.as_str())
            || path.starts_with('-')
            || !self.dir.join(command::unquote(path)).is_file()
        {
            return None;
        }
        let mut rest: Vec<&String> = tokens[2..].iter().collect();
        if rest.last().is_some_and(|t| command::is_file_list(t)) {
            rest.pop();
        }
        if rest
            .iter()
            .any(|t| command::is_file_list(t) || t.contains(['$', '`', '*', '?', '~']))
        {
            return None;
        }
        Some(Body::Script {
            path: command::unquote(path).to_string(),
            runner: Some(runner.clone()),
            args: rest.iter().map(|t| command::unquote(t).to_string()).collect(),
        })
    }
}

enum Resolved {
    Skip,
    Builtin(Recognized),
    Job(Box<JobSpec>),
}

struct Scope<'a> {
    stage: &'a str,
    label: &'a str,
    files: &'a [String],
    exclude: &'a [String],
    layer: i64,
    deepest: i64,
}

fn is_prek(spec: &JobSpec) -> bool {
    matches!(&spec.body, Body::Run(run) if {
        let tokens = command::tokenize(run);
        matches!(tokens.as_slice(), [program, sub, ..] if matches!(program.as_str(), "prek" | "pre-commit") && sub == "run")
    })
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn tags_of(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => s.split_whitespace().map(str::to_string).collect(),
        other => strings(other),
    }
}

fn derived_name(job: &Map<String, Value>) -> Option<String> {
    let run = job.get("run").and_then(Value::as_str)?;
    let program = command::tokenize(run).into_iter().next()?;
    Some(program.rsplit('/').next().unwrap_or(&program).to_string())
}

/// A TOML-key-safe job name.
fn sanitize(name: &str) -> String {
    let stem = [".sh", ".bash", ".py", ".js", ".rb"]
        .iter()
        .find_map(|ext| name.strip_suffix(ext))
        .unwrap_or(name);
    let cleaned: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    cleaned.trim_matches('_').to_string()
}
