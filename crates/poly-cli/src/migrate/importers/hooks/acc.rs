//! The shared accumulator both hook importers feed, and its rendering into
//! `poly.toml` fragments.
//!
//! A lefthook job and a pre-commit hook can land in the same poly builtin
//! (`fmt`, `lint`, `file_safety`), and a builtin carries a single `files` /
//! `exclude` set, so the contributions are merged here rather than per source.

use std::collections::{BTreeMap, BTreeSet};

use toml_edit::{Array, InlineTable, Item, Value};

use super::command::Target;
use crate::migrate::importers::{Fragment, bool_item, inline_map, int_item, str_array, str_item};

/// Which source file a diagnostic belongs to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    #[default]
    Lefthook,
    PreCommit,
    Shared,
}

/// Everything the importers learned that is not a clean one-to-one mapping.
#[derive(Debug, Default, Clone)]
pub(super) struct Diagnostics {
    /// The source the importer currently running is reading.
    pub origin: Origin,
    /// Informational: what was mapped and how.
    pub notes: Vec<(Origin, String)>,
    /// Constructs that were dropped, approximated or left unmigrated. Any entry
    /// keeps its source file.
    pub lossy: Vec<(Origin, String)>,
}

impl Diagnostics {
    pub fn note(&mut self, message: impl Into<String>) {
        self.notes.push((self.origin, message.into()));
    }

    pub fn lossy(&mut self, message: impl Into<String>) {
        self.lossy.push((self.origin, message.into()));
    }

    pub fn lossy_count(&self) -> usize {
        self.lossy.len()
    }

    fn shared_lossy(&mut self, messages: impl IntoIterator<Item = String>) {
        self.lossy.extend(messages.into_iter().map(|m| (Origin::Shared, m)));
    }

    /// Notes visible to `origin` (its own plus shared).
    pub fn notes_for(&self, origin: Origin) -> Vec<String> {
        select(&self.notes, origin)
    }

    /// Lossy entries visible to `origin` (its own plus shared).
    pub fn lossy_for(&self, origin: Origin) -> Vec<String> {
        select(&self.lossy, origin)
    }
}

fn select(entries: &[(Origin, String)], origin: Origin) -> Vec<String> {
    entries
        .iter()
        .filter(|(o, _)| *o == origin || *o == Origin::Shared)
        .map(|(_, text)| text.clone())
        .collect()
}

/// A `skip` / `only` guard in poly's representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Guard {
    Always(bool),
    Runs(Vec<String>),
}

/// The body of an inline job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Body {
    Run(String),
    Script {
        path: String,
        runner: Option<String>,
        args: Vec<String>,
    },
}

/// One inline job destined for `[hooks.<stage>.commands|scripts.<name>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JobSpec {
    pub name: String,
    pub body: Body,
    pub files: Vec<String>,
    pub exclude: Vec<String>,
    pub file_types: Vec<String>,
    pub stage_fixed: bool,
    pub workspace: bool,
    pub root: Option<String>,
    pub env: BTreeMap<String, String>,
    pub tags: Vec<String>,
    pub fail_text: Option<String>,
    pub interactive: bool,
    pub use_stdin: bool,
    pub priority: i64,
    pub skip: Option<Guard>,
    pub only: Option<Guard>,
}

impl JobSpec {
    pub fn new(name: String, body: Body) -> Self {
        JobSpec {
            name,
            body,
            files: Vec::new(),
            exclude: Vec::new(),
            file_types: Vec::new(),
            stage_fixed: false,
            workspace: false,
            root: None,
            env: BTreeMap::new(),
            tags: Vec::new(),
            fail_text: None,
            interactive: false,
            use_stdin: false,
            priority: 0,
            skip: None,
            only: None,
        }
    }
}

/// Per-stage settings and jobs.
#[derive(Debug, Default)]
pub(super) struct StageAcc {
    pub parallel: bool,
    pub piped: bool,
    pub follow: bool,
    pub fail_on_changes: bool,
    pub exclude_tags: Vec<String>,
    pub skip: Option<Guard>,
    pub only: Option<Guard>,
    pub jobs: Vec<JobSpec>,
}

/// File scope contributed by one source job to a builtin or catalog tool.
#[derive(Debug, Clone)]
struct Scope {
    label: String,
    files: Vec<String>,
    exclude: Vec<String>,
}

#[derive(Debug, Default)]
struct BuiltinAcc {
    stages: BTreeSet<String>,
    scopes: Vec<Scope>,
}

#[derive(Debug, Default)]
struct FileSafetyAcc {
    stages: BTreeSet<String>,
    members: BTreeSet<&'static str>,
    max_added_file_kb: Option<i64>,
    scopes: Vec<Scope>,
}

/// The `file_safety` members poly implements, in the order poly documents them.
pub(super) const FILE_SAFETY_MEMBERS: &[&str] = &[
    "merge_conflict",
    "added_large_files",
    "private_key",
    "case_conflict",
    "executables_have_shebangs",
    "shebang_scripts_are_executable",
];

/// Accumulates hook configuration from every source in one directory.
#[derive(Debug, Default)]
pub(super) struct HookAcc {
    stages: BTreeMap<String, StageAcc>,
    builtins: BTreeMap<Target, BuiltinAcc>,
    file_safety: FileSafetyAcc,
    tools: BTreeMap<String, BuiltinAcc>,
    pub diag: Diagnostics,
}

impl HookAcc {
    pub fn stage(&mut self, name: &str) -> &mut StageAcc {
        self.stages.entry(name.to_string()).or_default()
    }

    pub fn add_job(&mut self, stage: &str, job: JobSpec) {
        let jobs = &mut self.stage(stage).jobs;
        let mut candidate = job;
        let base = candidate.name.clone();
        let mut suffix = 2;
        while jobs.iter().any(|existing| existing.name == candidate.name) {
            candidate.name = format!("{base}-{suffix}");
            suffix += 1;
        }
        jobs.push(candidate);
    }

    pub fn add_builtin(
        &mut self,
        target: Target,
        stage: &str,
        label: &str,
        files: &[String],
        exclude: &[String],
        catalog_tools: &[&str],
    ) {
        let scope = Scope {
            label: label.to_string(),
            files: files.to_vec(),
            exclude: exclude.to_vec(),
        };
        let builtin = self.builtins.entry(target).or_default();
        builtin.stages.insert(stage.to_string());
        builtin.scopes.push(scope.clone());
        for tool in catalog_tools {
            let entry = self.tools.entry((*tool).to_string()).or_default();
            entry.stages.insert(stage.to_string());
            entry.scopes.push(scope.clone());
        }
    }

    pub fn add_file_safety(
        &mut self,
        member: &'static str,
        stage: &str,
        label: &str,
        files: &[String],
        exclude: &[String],
    ) {
        self.file_safety.stages.insert(stage.to_string());
        self.file_safety.members.insert(member);
        self.file_safety.scopes.push(Scope {
            label: label.to_string(),
            files: files.to_vec(),
            exclude: exclude.to_vec(),
        });
    }

    pub fn set_max_added_file_kb(&mut self, kb: i64) {
        self.file_safety.max_added_file_kb = Some(kb);
    }

    /// Record the exclusions that had to be dropped when several jobs shared one
    /// builtin. Call once, after every importer has run.
    pub fn settle(&mut self) {
        let mut lost = Vec::new();
        for (target, builtin) in &self.builtins {
            let kind = match target {
                Target::Fmt => "builtin fmt",
                Target::Lint => "builtin lint",
            };
            lost.extend(merge_scopes(kind, &builtin.scopes).2);
        }
        if !self.file_safety.members.is_empty() {
            lost.extend(merge_scopes("builtin file_safety", &self.file_safety.scopes).2);
        }
        for (name, tool) in &self.tools {
            lost.extend(merge_scopes(&format!("tools.{name}"), &tool.scopes).2);
        }
        self.diag.shared_lossy(lost);
    }

    /// Render the accumulated configuration into `poly.toml` fragments.
    pub fn into_fragments(self) -> (Vec<Fragment>, Diagnostics) {
        let mut fragments = Vec::new();
        let mut builtin_entries: Vec<(String, Item)> = Vec::new();
        for (target, builtin) in &self.builtins {
            let key = match target {
                Target::Fmt => "fmt",
                Target::Lint => "lint",
            };
            builtin_entries.push((key.to_string(), render_builtin(builtin, &[])));
        }
        if !self.file_safety.members.is_empty() {
            let extra = file_safety_entries(&self.file_safety);
            let item = render_builtin(&as_builtin(&self.file_safety), &extra);
            builtin_entries.push(("file_safety".to_string(), item));
        }
        if !builtin_entries.is_empty() {
            fragments.push(Fragment::new(&["hooks", "builtin"], builtin_entries));
        }
        for (stage, acc) in &self.stages {
            fragments.extend(stage_fragments(stage, acc));
        }
        for (name, tool) in &self.tools {
            fragments.push(Fragment::new(&["tools", name], tool_entries(tool)));
        }
        (fragments, self.diag)
    }
}

fn as_builtin(file_safety: &FileSafetyAcc) -> BuiltinAcc {
    BuiltinAcc {
        stages: file_safety.stages.clone(),
        scopes: file_safety.scopes.clone(),
    }
}

fn file_safety_entries(file_safety: &FileSafetyAcc) -> Vec<(&'static str, Value)> {
    let mut entries = Vec::new();
    if let Some(kb) = file_safety.max_added_file_kb {
        entries.push(("max_added_file_kb", Value::from(kb)));
    }
    for member in FILE_SAFETY_MEMBERS {
        if !file_safety.members.contains(member) {
            entries.push((*member, Value::from(false)));
        }
    }
    entries
}

fn string_array(items: &[String]) -> Value {
    let mut array = Array::new();
    for item in items {
        array.push(item.as_str());
    }
    Value::Array(array)
}

/// Merge scopes into one `files` / `exclude` pair. `files` is the union (or
/// absent when any contributor is unscoped); `exclude` is the intersection, so a
/// path one contributor still wanted checked is never hidden. The second value
/// lists the exclusions that had to be dropped.
fn merge_scopes(kind: &str, scopes: &[Scope]) -> (Vec<String>, Vec<String>, Option<String>) {
    let files = if scopes.iter().any(|s| s.files.is_empty()) {
        Vec::new()
    } else {
        union(scopes.iter().map(|s| s.files.as_slice()))
    };
    let all_excludes = union(scopes.iter().map(|s| s.exclude.as_slice()));
    let exclude: Vec<String> = all_excludes
        .iter()
        .filter(|pattern| scopes.iter().all(|s| s.exclude.contains(pattern)))
        .cloned()
        .collect();
    let dropped: Vec<&String> = all_excludes.iter().filter(|p| !exclude.contains(p)).collect();
    let lost = (!dropped.is_empty()).then(|| {
        let owners: Vec<&str> = scopes.iter().map(|s| s.label.as_str()).collect();
        format!(
            "{kind}: {} exclude pattern(s) dropped because one `exclude` serves every contributing job ({}); dropped: {}",
            dropped.len(),
            owners.join(", "),
            dropped.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ")
        )
    });
    (files, exclude, lost)
}

fn union<'a>(lists: impl Iterator<Item = &'a [String]>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for list in lists {
        for item in list {
            if !out.contains(item) {
                out.push(item.clone());
            }
        }
    }
    out
}

fn render_builtin(builtin: &BuiltinAcc, extra: &[(&'static str, Value)]) -> Item {
    let (files, exclude, _) = merge_scopes("", &builtin.scopes);
    let stages: Vec<String> = builtin.stages.iter().cloned().collect();
    let default_stage = stages == ["pre-commit"];
    if default_stage && files.is_empty() && exclude.is_empty() && extra.is_empty() {
        return bool_item(true);
    }
    let mut table = InlineTable::new();
    if !default_stage {
        table.insert("stages", string_array(&stages));
    }
    if !files.is_empty() {
        table.insert("files", string_array(&files));
    }
    if !exclude.is_empty() {
        table.insert("exclude", string_array(&exclude));
    }
    for (key, value) in extra {
        table.insert(*key, value.clone());
    }
    Item::Value(Value::InlineTable(table))
}

fn tool_entries(tool: &BuiltinAcc) -> Vec<(String, Item)> {
    let (files, exclude, _) = merge_scopes("", &tool.scopes);
    let mut entries = vec![("enabled".to_string(), bool_item(true))];
    if !files.is_empty() {
        entries.push(("files".to_string(), str_array(&files)));
    }
    if !exclude.is_empty() {
        entries.push(("exclude".to_string(), str_array(&exclude)));
    }
    entries
}

fn guard_item(guard: &Guard) -> Item {
    match guard {
        Guard::Always(value) => bool_item(*value),
        Guard::Runs(commands) => {
            let mut array = Array::new();
            for command in commands {
                let mut table = InlineTable::new();
                table.insert("run", Value::from(command.as_str()));
                array.push(Value::InlineTable(table));
            }
            Item::Value(Value::Array(array))
        }
    }
}

fn stage_fragments(stage: &str, acc: &StageAcc) -> Vec<Fragment> {
    let mut fragments = Vec::new();
    let mut entries: Vec<(String, Item)> = Vec::new();
    let flags = [
        ("parallel", acc.parallel),
        ("piped", acc.piped),
        ("follow", acc.follow),
        ("fail_on_changes", acc.fail_on_changes),
    ];
    for (key, value) in flags {
        if value {
            entries.push((key.to_string(), bool_item(true)));
        }
    }
    if !acc.exclude_tags.is_empty() {
        entries.push(("exclude_tags".to_string(), str_array(&acc.exclude_tags)));
    }
    if let Some(guard) = &acc.skip {
        entries.push(("skip".to_string(), guard_item(guard)));
    }
    if let Some(guard) = &acc.only {
        entries.push(("only".to_string(), guard_item(guard)));
    }
    if !entries.is_empty() {
        fragments.push(Fragment::new(&["hooks", stage], entries));
    }
    for job in &acc.jobs {
        let table = match job.body {
            Body::Run(_) => "commands",
            Body::Script { .. } => "scripts",
        };
        fragments.push(Fragment::new(&["hooks", stage, table, &job.name], job_entries(job)));
    }
    fragments
}

fn job_entries(job: &JobSpec) -> Vec<(String, Item)> {
    let mut entries: Vec<(String, Item)> = Vec::new();
    match &job.body {
        Body::Run(run) => entries.push(("run".to_string(), str_item(run))),
        Body::Script { path, runner, args } => {
            entries.push(("script".to_string(), str_item(path)));
            if let Some(runner) = runner {
                entries.push(("runner".to_string(), str_item(runner)));
            }
            if !args.is_empty() {
                entries.push(("args".to_string(), str_array(args)));
            }
        }
    }
    if !job.files.is_empty() {
        entries.push(("files".to_string(), str_array(&job.files)));
    }
    if !job.exclude.is_empty() {
        entries.push(("exclude".to_string(), str_array(&job.exclude)));
    }
    if !job.file_types.is_empty() {
        entries.push(("file_types".to_string(), str_array(&job.file_types)));
    }
    let flags = [
        ("workspace", job.workspace),
        ("stage_fixed", job.stage_fixed),
        ("interactive", job.interactive),
        ("use_stdin", job.use_stdin),
    ];
    for (key, value) in flags {
        if value {
            entries.push((key.to_string(), bool_item(true)));
        }
    }
    if job.priority != 0 {
        entries.push(("priority".to_string(), int_item(job.priority)));
    }
    if let Some(root) = &job.root {
        entries.push(("root".to_string(), str_item(root)));
    }
    if !job.env.is_empty() {
        entries.push(("env".to_string(), inline_map(&job.env)));
    }
    if !job.tags.is_empty() {
        entries.push(("tags".to_string(), str_array(&job.tags)));
    }
    if let Some(text) = &job.fail_text {
        entries.push(("fail_text".to_string(), str_item(text)));
    }
    if let Some(guard) = &job.skip {
        entries.push(("skip".to_string(), guard_item(guard)));
    }
    if let Some(guard) = &job.only {
        entries.push(("only".to_string(), guard_item(guard)));
    }
    entries
}
