//! The shared accumulator both hook importers feed, and its rendering into
//! `poly.toml` fragments.
//!
//! A lefthook job and a pre-commit hook can land in the same poly builtin
//! (`fmt`, `lint`, `file_safety`), and a builtin carries a single `files` /
//! `exclude` set, so the contributions are merged here rather than per source.

use std::collections::{BTreeMap, BTreeSet};

use toml_edit::{Array, InlineTable, Item, Value};

use super::command::{EngineOption, EngineValue, Target};
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

    pub fn shared_notes(&mut self, messages: impl IntoIterator<Item = String>) {
        self.notes.extend(messages.into_iter().map(|m| (Origin::Shared, m)));
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

/// What a job is known to do to the files it is given.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum Effect {
    /// Not classified; the job keeps the priority its source gave it.
    #[default]
    Unknown,
    /// Only reports on the files, so it must run after every fixer.
    Reports,
    /// Rewrites the files.
    Rewrites,
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
    pub effect: Effect,
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
            effect: Effect::Unknown,
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

/// What distinguishes one run of a builtin from another.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Shape {
    pub files: Vec<String>,
    pub exclude: Vec<String>,
    pub fix: bool,
    pub priority: i64,
}

/// What a migrated hook says about how a catalog tool should run, carried to
/// `[tools.<name>]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ToolSettings {
    /// Flags for every role (`-c checkstyle.xml`, `-ignore SC2129`).
    pub extra_args: Vec<String>,
    /// Flags for the formatting role only (`buildifier -lint=fix`).
    pub format_extra_args: Vec<String>,
    /// Flags for the linting role only.
    pub lint_extra_args: Vec<String>,
    /// A named catalog command (`palantir-java-format`'s `aosp` pipeline).
    pub command: Option<String>,
    /// The pinned release the hook ran; when set, poly may download it.
    pub version: Option<String>,
    /// Report only findings new since this git revision (`--new-from-rev`).
    pub new_from_rev: Option<String>,
}

impl ToolSettings {
    pub(super) fn is_empty(&self) -> bool {
        self == &ToolSettings::default()
    }

    /// Fold `other` in. A repeated sequence is kept once; a different sequence
    /// for the same slot is a conflict, returned as the slot's name.
    fn merge(&mut self, other: &ToolSettings) -> Vec<&'static str> {
        let mut conflicts = Vec::new();
        let slots = [
            ("extra_args", &mut self.extra_args, &other.extra_args),
            (
                "format_extra_args",
                &mut self.format_extra_args,
                &other.format_extra_args,
            ),
            ("lint_extra_args", &mut self.lint_extra_args, &other.lint_extra_args),
        ];
        for (name, mine, theirs) in slots {
            if theirs.is_empty() || *mine == *theirs {
                continue;
            }
            if mine.is_empty() {
                mine.clone_from(theirs);
            } else {
                conflicts.push(name);
            }
        }
        for (name, mine, theirs) in [
            ("command", &mut self.command, &other.command),
            ("version", &mut self.version, &other.version),
            ("new_from_rev", &mut self.new_from_rev, &other.new_from_rev),
        ] {
            match (&*mine, theirs) {
                (_, None) => {}
                (None, Some(_)) => mine.clone_from(theirs),
                (Some(a), Some(b)) if a != b => conflicts.push(name),
                _ => {}
            }
        }
        conflicts
    }
}

/// One source job's use of a builtin or catalog tool.
#[derive(Debug, Clone)]
struct Scope {
    label: String,
    stage: String,
    shape: Shape,
}

/// A source job that maps onto a poly builtin.
pub(super) struct BuiltinUse<'a> {
    pub target: Target,
    pub stage: &'a str,
    pub label: &'a str,
    pub shape: Shape,
    pub catalog_tools: &'a [&'a str],
    pub options: &'a [EngineOption],
}

#[derive(Debug, Default)]
struct ToolAcc {
    stages: BTreeSet<String>,
    scopes: Vec<Scope>,
}

#[derive(Debug, Default)]
struct FileSafetyAcc {
    scopes: Vec<(Scope, &'static str)>,
    max_added_file_kb: Option<i64>,
}

/// The `file_safety` members poly implements, in the order poly documents them.
pub(super) const FILE_SAFETY_MEMBERS: &[&str] = &[
    "merge_conflict",
    "added_large_files",
    "private_key",
    "case_conflict",
    "executables_have_shebangs",
    "shebang_scripts_are_executable",
    "aws_credentials",
    "requirements_txt_fixer",
];

/// Members that are off unless named, rather than on unless disabled.
const OPT_IN_MEMBERS: &[&str] = &["aws_credentials", "requirements_txt_fixer"];

/// Accumulates hook configuration from every source in one directory.
#[derive(Debug, Default)]
pub(super) struct HookAcc {
    stages: BTreeMap<String, StageAcc>,
    builtins: BTreeMap<Target, Vec<Scope>>,
    file_safety: FileSafetyAcc,
    tools: BTreeMap<String, ToolAcc>,
    engine_options: BTreeMap<Vec<String>, BTreeSet<&'static str>>,
    engine_values: BTreeMap<Vec<String>, BTreeMap<&'static str, i64>>,
    tool_settings: BTreeMap<String, ToolSettings>,
    pub diag: Diagnostics,
}

/// One run of a builtin: where it runs, on what, and which checks it carries.
#[derive(Debug)]
struct Group {
    stages: BTreeSet<String>,
    shape: Shape,
    members: BTreeSet<&'static str>,
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

    pub fn add_builtin(&mut self, usage: BuiltinUse<'_>) {
        let scope = Scope {
            label: usage.label.to_string(),
            stage: usage.stage.to_string(),
            shape: usage.shape,
        };
        for tool in usage.catalog_tools {
            let entry = self.tools.entry((*tool).to_string()).or_default();
            entry.stages.insert(usage.stage.to_string());
            entry.scopes.push(scope.clone());
        }
        for option in usage.options {
            let table = option.table.iter().map(|segment| (*segment).to_string()).collect();
            self.engine_options.entry(table).or_default().insert(option.key);
        }
        self.builtins.entry(usage.target).or_default().push(scope);
    }

    /// Record a boolean engine option switched on by the hook's flags.
    pub fn add_engine_option(&mut self, option: &EngineOption) {
        let table = option.table.iter().map(|segment| (*segment).to_string()).collect();
        self.engine_options.entry(table).or_default().insert(option.key);
    }

    /// Record an integer engine option set by the hook labelled `label`; when another hook
    /// set it differently the first value is kept and the difference reported.
    pub fn add_engine_value(&mut self, value: &EngineValue, label: &str) {
        let table = value.table.iter().map(|segment| (*segment).to_string()).collect();
        let slot = self
            .engine_values
            .entry(table)
            .or_default()
            .entry(value.key)
            .or_insert(value.value);
        if *slot != value.value {
            let (kept, key) = (*slot, value.key);
            self.diag.lossy(format!(
                "{label}: {} {key} = {} differs from what another hook set ({kept}); the first was kept",
                value.table.join("."),
                value.value
            ));
        }
    }

    /// Record how `tool` should be configured, from the hook labelled `label`.
    pub fn add_tool_settings(&mut self, tool: &str, label: &str, settings: &ToolSettings) {
        if settings.is_empty() {
            return;
        }
        let entry = self.tool_settings.entry(tool.to_string()).or_default();
        for slot in entry.merge(settings) {
            self.diag.lossy(format!(
                "{label}: [tools.{tool}] {slot} differs from what another hook set; the first was kept"
            ));
        }
    }

    pub fn add_file_safety(&mut self, member: &'static str, stage: &str, label: &str, shape: Shape) {
        let scope = Scope {
            label: label.to_string(),
            stage: stage.to_string(),
            shape,
        };
        self.file_safety.scopes.push((scope, member));
    }

    pub fn set_max_added_file_kb(&mut self, kb: i64) {
        self.file_safety.max_added_file_kb = Some(kb);
    }

    /// Move every check-only run behind the last fixer of its stage, so a check
    /// never flags something a fixer repairs a moment later: lefthook runs its
    /// `fix` group before its `check` group, and pre-commit runs hooks in order.
    /// Runs already placed after the last fixer keep their priority.
    pub fn order_checks_after_fixers(&mut self) {
        let mut last_fixer: BTreeMap<String, i64> = BTreeMap::new();
        let mut note_fixer = |stage: &str, priority: i64| {
            let entry = last_fixer.entry(stage.to_string()).or_insert(priority);
            *entry = (*entry).max(priority);
        };
        for scope in self.builtins.values().flatten().filter(|scope| scope.shape.fix) {
            note_fixer(&scope.stage, scope.shape.priority);
        }
        for (stage, acc) in &self.stages {
            for job in &acc.jobs {
                if job.stage_fixed || job.effect == Effect::Rewrites {
                    note_fixer(stage, job.priority);
                }
            }
        }
        for scope in self.builtins.values_mut().flatten().filter(|scope| !scope.shape.fix) {
            if let Some(floor) = last_fixer.get(&scope.stage) {
                scope.shape.priority = scope.shape.priority.max(floor + 1);
            }
        }
        for (stage, acc) in &mut self.stages {
            let Some(floor) = last_fixer.get(stage) else {
                continue;
            };
            for job in acc.jobs.iter_mut().filter(|job| job.effect == Effect::Reports) {
                job.priority = job.priority.max(floor + 1);
            }
        }
    }

    /// Report what a shared catalog tool could not keep. A builtin's scope lives
    /// on its own hook, so nothing is lost there; the `[tools.<name>]` table has
    /// one `exclude` for every use, which only matters outside the hooks.
    pub fn settle(&mut self) {
        let mut notes = Vec::new();
        for (name, tool) in &self.tools {
            let (_, _, dropped) = merge_tool_scopes(&tool.scopes);
            if !dropped.is_empty() {
                let owners: Vec<&str> = tool.scopes.iter().map(|scope| scope.label.as_str()).collect();
                notes.push(format!(
                    "tools.{name}: {} exclude pattern(s) are not in [tools.{name}] because the jobs using it ({}) \
                     exclude different paths; each job keeps its own exclude on its hook, but a direct `poly lint` \
                     run uses only the shared set; not in [tools.{name}]: {}",
                    dropped.len(),
                    owners.join(", "),
                    dropped.join(", ")
                ));
            }
        }
        self.diag.shared_notes(notes);
    }

    /// Render the accumulated configuration into `poly.toml` fragments.
    pub fn into_fragments(self) -> (Vec<Fragment>, Diagnostics) {
        let mut fragments = Vec::new();
        let mut builtin_entries: Vec<(String, Item)> = Vec::new();
        for (target, scopes) in &self.builtins {
            let key = match target {
                Target::Fmt => "fmt",
                Target::Lint => "lint",
            };
            let groups = groups(scopes.iter().map(|scope| (scope, None)));
            builtin_entries.push((key.to_string(), render_groups(&groups, false, &[])));
        }
        if !self.file_safety.scopes.is_empty() {
            let groups = groups(
                self.file_safety
                    .scopes
                    .iter()
                    .map(|(scope, member)| (scope, Some(*member))),
            );
            let extra = file_safety_entries(&self.file_safety);
            builtin_entries.push(("file_safety".to_string(), render_groups(&groups, true, &extra)));
        }
        if !builtin_entries.is_empty() {
            fragments.push(Fragment::new(&["hooks", "builtin"], builtin_entries));
        }
        for (stage, acc) in &self.stages {
            fragments.extend(stage_fragments(stage, acc));
        }
        for (name, tool) in &self.tools {
            let settings = self.tool_settings.get(name).cloned().unwrap_or_default();
            fragments.push(Fragment::new(&["tools", name], tool_entries(tool, &settings)));
        }
        let tables: BTreeSet<&Vec<String>> = self.engine_options.keys().chain(self.engine_values.keys()).collect();
        for table in tables {
            let path: Vec<&str> = table.iter().map(String::as_str).collect();
            let mut entries: Vec<(String, Item)> = self
                .engine_options
                .get(table)
                .into_iter()
                .flatten()
                .map(|key| ((*key).to_string(), bool_item(true)))
                .collect();
            entries.extend(
                self.engine_values
                    .get(table)
                    .into_iter()
                    .flatten()
                    .map(|(key, value)| ((*key).to_string(), int_item(*value))),
            );
            fragments.push(Fragment::new(&path, entries));
        }
        (fragments, self.diag)
    }
}

/// Split uses of a builtin into the distinct runs poly needs: one per scope, with
/// the stages and checks that share it combined.
fn groups<'a>(entries: impl Iterator<Item = (&'a Scope, Option<&'static str>)>) -> Vec<Group> {
    let mut per_stage: BTreeMap<(Shape, String), BTreeSet<&'static str>> = BTreeMap::new();
    for (scope, member) in entries {
        let members = per_stage.entry((scope.shape.clone(), scope.stage.clone())).or_default();
        members.extend(member);
    }
    let mut merged: BTreeMap<(Shape, BTreeSet<&'static str>), BTreeSet<String>> = BTreeMap::new();
    for ((shape, stage), members) in per_stage {
        merged.entry((shape, members)).or_default().insert(stage);
    }
    merged
        .into_iter()
        .map(|((shape, members), stages)| Group { stages, shape, members })
        .collect()
}

fn file_safety_entries(file_safety: &FileSafetyAcc) -> Vec<(&'static str, Value)> {
    let used: BTreeSet<&str> = file_safety.scopes.iter().map(|(_, member)| *member).collect();
    let mut entries = Vec::new();
    if let Some(kb) = file_safety.max_added_file_kb {
        entries.push(("max_added_file_kb", Value::from(kb)));
    }
    for member in FILE_SAFETY_MEMBERS {
        let opt_in = OPT_IN_MEMBERS.contains(member);
        if used.contains(member) == opt_in {
            entries.push((*member, Value::from(opt_in)));
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

fn str_list<'a>(items: impl IntoIterator<Item = &'a str>) -> Value {
    let mut array = Array::new();
    for item in items {
        array.push(item);
    }
    Value::Array(array)
}

/// The keys of one run, as they appear either on the builtin or in a `jobs` entry.
fn group_table(group: &Group, with_checks: bool) -> InlineTable {
    let mut table = InlineTable::new();
    let default_stage = group.stages.len() == 1 && group.stages.contains("pre-commit");
    if !default_stage {
        table.insert("stages", str_list(group.stages.iter().map(String::as_str)));
    }
    if with_checks {
        table.insert("checks", str_list(group.members.iter().copied()));
    }
    if !group.shape.files.is_empty() {
        table.insert("files", string_array(&group.shape.files));
    }
    if !group.shape.exclude.is_empty() {
        table.insert("exclude", string_array(&group.shape.exclude));
    }
    if group.shape.fix {
        table.insert("fix", Value::from(true));
    }
    if group.shape.priority != 0 {
        table.insert("priority", Value::from(group.shape.priority));
    }
    table
}

/// A builtin with one run is a bare `true` or a flat table; with several, a
/// `jobs` list so every run keeps its own `files` / `exclude` / `fix`.
fn render_groups(groups: &[Group], with_checks: bool, extra: &[(&'static str, Value)]) -> Item {
    let mut table = match groups {
        [only] => group_table(only, false),
        many => {
            let mut array = Array::new();
            for group in many {
                let mut entry = Value::InlineTable(group_table(group, with_checks));
                entry.decor_mut().set_prefix("\n  ");
                array.push_formatted(entry);
            }
            array.set_trailing("\n");
            array.set_trailing_comma(true);
            let mut table = InlineTable::new();
            table.insert("jobs", Value::Array(array));
            table
        }
    };
    if table.is_empty() && extra.is_empty() {
        return bool_item(true);
    }
    for (key, value) in extra {
        table.insert(*key, value.clone());
    }
    Item::Value(Value::InlineTable(table))
}

/// Merge a catalog tool's uses into the single `files` / `exclude` its table
/// holds: `files` is the union (absent when any use is unscoped), `exclude` the
/// intersection, so a path one use still wanted checked is never hidden. The last
/// value lists the exclusions the intersection dropped.
fn merge_tool_scopes(scopes: &[Scope]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let files = if scopes.iter().any(|s| s.shape.files.is_empty()) {
        Vec::new()
    } else {
        union(scopes.iter().map(|s| s.shape.files.as_slice()))
    };
    let all_excludes = union(scopes.iter().map(|s| s.shape.exclude.as_slice()));
    let exclude: Vec<String> = all_excludes
        .iter()
        .filter(|pattern| scopes.iter().all(|s| s.shape.exclude.contains(pattern)))
        .cloned()
        .collect();
    let dropped = all_excludes.iter().filter(|p| !exclude.contains(p)).cloned().collect();
    (files, exclude, dropped)
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

fn tool_entries(tool: &ToolAcc, settings: &ToolSettings) -> Vec<(String, Item)> {
    let (files, exclude, _) = merge_tool_scopes(&tool.scopes);
    let mut entries = vec![("enabled".to_string(), bool_item(true))];
    if let Some(command) = &settings.command {
        entries.push(("command".to_string(), str_item(command)));
    }
    if let Some(version) = &settings.version {
        entries.push(("version".to_string(), str_item(version)));
        entries.push(("install".to_string(), str_item("auto")));
    }
    if let Some(rev) = &settings.new_from_rev {
        entries.push(("new_from_rev".to_string(), str_item(rev)));
    }
    if !files.is_empty() {
        entries.push(("files".to_string(), str_array(&files)));
    }
    if !exclude.is_empty() {
        entries.push(("exclude".to_string(), str_array(&exclude)));
    }
    for (key, args) in [
        ("extra_args", &settings.extra_args),
        ("format_extra_args", &settings.format_extra_args),
        ("lint_extra_args", &settings.lint_extra_args),
    ] {
        if !args.is_empty() {
            entries.push((key.to_string(), str_array(args)));
        }
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
