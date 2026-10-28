//! Linting with a recipe tool: group the files into invocations, run them, and
//! hand every finding to the file it belongs to.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::anyhow;
use poly_catalog::{Findings, Group};
use rayon::prelude::*;

use super::argv::{Items, Template};
use super::mirror::Mirror;
use super::output::{self, Finding};
use super::{RecipeTool, exec, paths, unit};
use crate::engine::{Diagnostic, Edit, SourceFile, Span};

/// One answer per input file, in order.
pub(in crate::engines::catalog_tool) type LintResults = Vec<anyhow::Result<Vec<Diagnostic>>>;

/// A file of the run, located relative to the working directory.
pub(super) struct Member<'a> {
    pub(super) src: &'a SourceFile,
    pub(super) rel: PathBuf,
}

/// The files that share one invocation root.
struct UnitGroup {
    /// Where the tool is run from.
    root: PathBuf,
    /// Indices into the member list.
    members: Vec<usize>,
    /// Each member's path relative to `root`, slashed.
    rels: Vec<String>,
    /// `root` relative to the tree's base, slashed (`.` for the base itself).
    dir: String,
    /// A chart in `skip_charts`: not linted, only run to check the skip still holds.
    skipped: bool,
}

#[derive(Default)]
struct Tally {
    accepted: usize,
    refused: usize,
    abandoned: Option<String>,
}

/// What one task (a chunk of a group) came to.
type Outcome = Result<Arc<Vec<Finding>>, String>;

struct Task {
    group: usize,
    /// The range of the group's items this task covers.
    items: Range<usize>,
}

impl RecipeTool {
    /// Lint `files`, one result per file.
    pub(in crate::engines::catalog_tool) fn lint_files(&self, files: &[&SourceFile]) -> LintResults {
        let members = self.locate(files);
        let mut results: Vec<Option<anyhow::Result<Vec<Diagnostic>>>> = files.iter().map(|_| None).collect();

        let (on_disk, diverged): (Vec<usize>, Vec<usize>) =
            (0..files.len()).partition(|&i| self.matches_disk(&members[i]));
        self.lint_tree(&self.cwd, &members, &on_disk, &mut results);
        if !diverged.is_empty() {
            match self.lint_mirrored(&members, &diverged) {
                Ok(done) => {
                    for (index, outcome) in done {
                        results[index] = Some(outcome);
                    }
                }
                Err(error) => {
                    for index in diverged {
                        results[index] = Some(Err(anyhow!("{error:#}")));
                    }
                }
            }
        }
        self.attach_fixes(&members, &mut results);
        results
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Ok(Vec::new())))
            .collect()
    }

    /// Place each file relative to the working directory. A path outside it gets
    /// a stable stand-in so it can still be mirrored.
    pub(super) fn locate<'a>(&self, files: &[&'a SourceFile]) -> Vec<Member<'a>> {
        files
            .iter()
            .enumerate()
            .map(|(index, src)| {
                let rel = paths::relative(&self.cwd, &src.path).unwrap_or_else(|| {
                    let name = src.path.file_name().unwrap_or_default();
                    PathBuf::from("_outside").join(index.to_string()).join(name)
                });
                Member { src, rel }
            })
            .collect()
    }

    fn matches_disk(&self, member: &Member<'_>) -> bool {
        std::fs::read(self.cwd.join(&member.rel)).is_ok_and(|bytes| bytes == member.src.content.as_bytes())
    }

    /// Lint the content poly holds when it differs from disk, from a scratch
    /// copy with the project context copied beside it.
    fn lint_mirrored(
        &self,
        members: &[Member<'_>],
        indices: &[usize],
    ) -> anyhow::Result<Vec<(usize, anyhow::Result<Vec<Diagnostic>>)>> {
        let mirror = Mirror::new()?;
        for &index in indices {
            let member = &members[index];
            mirror.write(&member.rel, &member.src.content)?;
            mirror.copy_context(&self.cwd, &member.rel, &self.context_names);
        }
        let mut results: Vec<Option<anyhow::Result<Vec<Diagnostic>>>> = members.iter().map(|_| None).collect();
        self.lint_tree(mirror.root(), members, indices, &mut results);
        Ok(indices
            .iter()
            .map(|&i| (i, results[i].take().unwrap_or_else(|| Ok(Vec::new()))))
            .collect())
    }

    /// Lint the members at `indices`, whose files live under `base`.
    fn lint_tree(
        &self,
        base: &Path,
        members: &[Member<'_>],
        indices: &[usize],
        results: &mut [Option<anyhow::Result<Vec<Diagnostic>>>],
    ) {
        let Some(template) = self.passes.first() else {
            return;
        };
        let groups = self.plan(base, members, indices);
        if base == self.cwd.as_path() {
            groups.par_iter().for_each(|unit| self.prepare(unit));
        }
        let mut findings: Vec<Vec<Finding>> = members.iter().map(|_| Vec::new()).collect();
        let mut failures: Vec<Option<String>> = members.iter().map(|_| None).collect();
        let lookup = Lookup::new(base);

        let tasks: Vec<Task> = groups
            .iter()
            .enumerate()
            .filter(|(_, unit)| !unit.skipped)
            .flat_map(|(group, unit)| {
                let items = self.items_of(template, unit, members);
                template
                    .chunks(&items)
                    .into_iter()
                    .map(move |range| Task { group, items: range })
            })
            .collect();

        let run = |task: &Task| -> Vec<(Range<usize>, Outcome)> {
            let unit = &groups[task.group];
            let items = self.items_of(template, unit, members);
            self.run_range(template, unit, &items, task.items.clone())
        };
        let done: Vec<Vec<(Range<usize>, Outcome)>> = if self.recipe.serial {
            tasks.iter().map(run).collect()
        } else {
            tasks.par_iter().map(run).collect()
        };

        for (task, outcomes) in tasks.iter().zip(done) {
            let unit = &groups[task.group];
            let items = self.items_of(template, unit, members);
            for (range, outcome) in outcomes {
                let covered = self.covered(template, unit, members, &items, &range);
                match outcome {
                    Ok(batch) => self.attribute(&batch, unit, &covered, members, &lookup, &mut findings),
                    Err(reason) => {
                        for index in covered {
                            failures[index] = Some(reason.clone());
                        }
                    }
                }
            }
        }

        let stale: Vec<(&UnitGroup, String)> = groups
            .par_iter()
            .filter(|unit| unit.skipped)
            .filter_map(|unit| self.stale_skip(template, unit, members).map(|reason| (unit, reason)))
            .collect();
        for (unit, reason) in stale {
            for &index in &unit.members {
                failures[index] = Some(reason.clone());
            }
        }

        for &index in indices {
            results[index] = Some(match failures[index].take() {
                Some(reason) => Err(anyhow!("{} failed: {reason}", self.tool.name)),
                None => Ok(findings[index].drain(..).map(|f| self.diagnostic(&f)).collect()),
            });
        }
    }

    /// Split the members into invocation roots.
    fn plan(&self, base: &Path, members: &[Member<'_>], indices: &[usize]) -> Vec<UnitGroup> {
        let mut groups: BTreeMap<PathBuf, UnitGroup> = BTreeMap::new();
        for &index in indices {
            let abs = base.join(&members[index].rel);
            let root = match &self.recipe.group {
                Group::None | Group::Dir => base.to_path_buf(),
                Group::Marker(markers) => match paths::project_root(&abs, markers) {
                    Some(root) => root,
                    None => {
                        tracing::debug!(tool = %self.tool.name, file = %abs.display(), "no project root found; skipping");
                        continue;
                    }
                },
            };
            let key = match &self.recipe.group {
                Group::Dir => abs.parent().map(Path::to_path_buf).unwrap_or_else(|| root.clone()),
                _ => root.clone(),
            };
            let rel = abs
                .strip_prefix(&root)
                .map_or_else(|_| members[index].rel.clone(), Path::to_path_buf);
            let dir = root
                .strip_prefix(base)
                .map_or_else(|_| ".".to_owned(), paths::slashed);
            let dir = if dir.is_empty() { ".".to_owned() } else { dir };
            let skipped = self.units.skips(&dir);
            if skipped && !self.units.verifies_skips() {
                continue;
            }
            let unit = groups.entry(key).or_insert_with(|| UnitGroup {
                root: root.clone(),
                members: Vec::new(),
                rels: Vec::new(),
                dir,
                skipped,
            });
            unit.members.push(index);
            unit.rels.push(paths::slashed(&rel));
        }
        groups.into_values().collect()
    }

    /// The failure to report for a skipped chart the tool now succeeds on: the
    /// skip is stale and should be removed, as it would otherwise outlive its
    /// reason unnoticed.
    fn stale_skip(&self, template: &Template, unit: &UnitGroup, members: &[Member<'_>]) -> Option<String> {
        let mut argv = template.render(&self.items_of(template, unit, members));
        argv.extend(self.units.extra_arguments(&unit.dir));
        let captured = exec::run(&self.launcher, &argv, &unit.root, &self.env, self.unset_env).ok()?;
        captured.exited_ok(&self.recipe.ok_exits).then(|| {
            format!(
                "{}: succeeds, but is listed in skip_charts; remove it from `skip_charts` of [tools.{}]",
                unit.dir, self.tool.name
            )
        })
    }

    /// Make the root checkable before the tool runs on it (`helm dependency
    /// build`). Best effort: a tool judges a root with missing dependencies
    /// itself, and says which, so a failed build is logged and the run goes on.
    fn prepare(&self, unit: &UnitGroup) {
        let Some(argv) = self.units.prepare_argv() else {
            return;
        };
        if unit::offline() || unit::dependencies_in_place(&unit.root) {
            return;
        }
        let outcome = unit::once_per_root(&unit.root, argv, || {
            self.units.retry().run(
                || {
                    let captured = exec::run(&self.launcher, argv, &unit.root, &self.env, &[])
                        .map_err(|error| format!("{error:#}"))?;
                    if captured.exited_ok(&self.recipe.ok_exits) {
                        Ok(())
                    } else {
                        Err(captured.explanation())
                    }
                },
                std::thread::sleep,
            )
        });
        if let Err(reason) = outcome {
            tracing::warn!(
                tool = %self.tool.name,
                chart = %unit.dir,
                "`{}` failed, linting with the dependencies on disk: {reason}",
                argv.join(" ")
            );
        }
    }

    fn is_go_module_tool(&self) -> bool {
        matches!(&self.recipe.group, Group::Marker(markers) if markers.iter().any(|m| m == "go.mod"))
    }

    /// What the template spends: the group's files, their directories, or nothing.
    fn items_of(&self, template: &Template, unit: &UnitGroup, _members: &[Member<'_>]) -> Vec<String> {
        match template.items() {
            Items::Files => unit.rels.clone(),
            Items::Dirs => {
                let mut dirs: Vec<String> = unit
                    .rels
                    .iter()
                    .filter(|rel| !(self.is_go_module_tool() && is_ignored_by_go_tooling(rel)))
                    .map(|rel| directory_of(rel))
                    .collect();
                dirs.sort();
                dirs.dedup();
                dirs
            }
            Items::Nothing => Vec::new(),
        }
    }

    /// The member indices an item range answers for.
    fn covered(
        &self,
        template: &Template,
        unit: &UnitGroup,
        _members: &[Member<'_>],
        items: &[String],
        range: &Range<usize>,
    ) -> Vec<usize> {
        match template.items() {
            Items::Files => unit.members[range.clone()].to_vec(),
            Items::Dirs => unit
                .rels
                .iter()
                .zip(&unit.members)
                .filter(|(rel, _)| items[range.clone()].contains(&directory_of(rel)))
                .map(|(_, &index)| index)
                .collect(),
            Items::Nothing => unit.members.clone(),
        }
    }

    /// Run `range` of the group's items; when the tool fails on it, halve until
    /// the failure is pinned on single items. A tool that refuses two single
    /// items before accepting anything is being invoked wrongly, so the rest is
    /// abandoned instead of halved into a spawn per file.
    fn run_range(
        &self,
        template: &Template,
        unit: &UnitGroup,
        items: &[String],
        range: Range<usize>,
    ) -> Vec<(Range<usize>, Outcome)> {
        let mut tally = Tally::default();
        let mut out = Vec::new();
        self.run_halving(template, unit, items, range, &mut tally, &mut out);
        out
    }

    fn run_halving(
        &self,
        template: &Template,
        unit: &UnitGroup,
        items: &[String],
        range: Range<usize>,
        tally: &mut Tally,
        out: &mut Vec<(Range<usize>, Outcome)>,
    ) {
        if let Some(reason) = &tally.abandoned {
            out.push((range, Err(reason.clone())));
            return;
        }
        let outcome = self.invoke(template, unit, &items[range.clone()]);
        match outcome {
            Ok(_) => {
                tally.accepted += 1;
                out.push((range, outcome));
            }
            Err(reason) if range.len() < 2 || template.items() == Items::Nothing => {
                tally.refused += 1;
                if tally.accepted == 0 && tally.refused >= 2 {
                    tally.abandoned = Some(reason.clone());
                }
                out.push((range, Err(reason)));
            }
            Err(_) => {
                let middle = range.start + range.len() / 2;
                self.run_halving(template, unit, items, range.start..middle, tally, out);
                self.run_halving(template, unit, items, middle..range.end, tally, out);
            }
        }
    }

    /// One invocation, memoized for the process so the same project root asked
    /// about from several files (or several shards) is run once.
    fn invoke(&self, template: &Template, unit: &UnitGroup, items: &[String]) -> Outcome {
        type Cell = Arc<OnceLock<Outcome>>;
        static MEMO: OnceLock<Mutex<HashMap<String, Cell>>> = OnceLock::new();
        let mut argv = template.render(items);
        argv.extend(self.units.extra_arguments(&unit.dir));
        let key = format!(
            "{}\u{1e}{}\u{1e}{}\u{1e}{}",
            self.launcher.identity,
            unit.root.display(),
            argv.join("\u{1f}"),
            self.version
        );
        let cell = {
            let mut memo = MEMO
                .get_or_init(Default::default)
                .lock()
                .expect("invocation memo poisoned");
            Arc::clone(memo.entry(key).or_default())
        };
        cell.get_or_init(|| self.execute(&argv, &unit.root)).clone()
    }

    /// Run once, or once per configured `GOOS` with the findings merged.
    fn execute(&self, argv: &[String], root: &Path) -> Outcome {
        if self.goos.is_empty() {
            return self.execute_with(argv, root, &self.env);
        }
        let mut merged: Vec<Finding> = Vec::new();
        for goos in &self.goos {
            let mut env = self.env.clone();
            env.insert("GOOS".to_owned(), goos.clone());
            let found = self
                .execute_with(argv, root, &env)
                .map_err(|reason| format!("GOOS={goos}: {reason}"))?;
            for finding in found.iter() {
                if !merged.contains(finding) {
                    merged.push(finding.clone());
                }
            }
        }
        Ok(Arc::new(merged))
    }

    fn execute_with(&self, argv: &[String], root: &Path, env: &BTreeMap<String, String>) -> Outcome {
        let captured =
            exec::run(&self.launcher, argv, root, env, self.unset_env).map_err(|error| format!("{error:#}"))?;
        match output::parse(self.recipe.output, &captured.stdout, &captured.stderr) {
            // A failing exit that reported nothing is a tool that did not run to
            // completion, not a clean file.
            Ok(findings) if findings.is_empty() && !captured.exited_ok(&self.recipe.ok_exits) => {
                Err(captured.explanation())
            }
            Ok(findings) => Ok(Arc::new(findings)),
            Err(error) => Err(format!("{error:#} ({})", captured.explanation())),
        }
    }

    /// Hand each finding of one invocation to its file.
    fn attribute(
        &self,
        batch: &[Finding],
        unit: &UnitGroup,
        covered: &[usize],
        members: &[Member<'_>],
        lookup: &Lookup,
        findings: &mut [Vec<Finding>],
    ) {
        let owners: HashMap<PathBuf, usize> = covered
            .iter()
            .flat_map(|&index| {
                lookup
                    .keys(&members[index].rel)
                    .into_iter()
                    .map(move |key| (key, index))
            })
            .collect();
        // Tools run with `--chdir` print paths relative to that directory.
        let mut hints: Vec<PathBuf> = covered
            .iter()
            .filter_map(|&index| members[index].rel.parent().map(Path::to_path_buf))
            .collect();
        hints.sort();
        hints.dedup();
        for finding in batch {
            let owner = finding
                .path
                .as_deref()
                .and_then(|raw| lookup.resolve(raw, &unit.root, &hints, &owners));
            match (owner, self.recipe.findings) {
                (Some(index), _) => findings[index].push(finding.clone()),
                (None, Findings::All) => {
                    // Every file of the run gets the finding, so a file's verdict is a
                    // function of the root's digest alone (`RecipeTool::cache_salt`) and
                    // never of which sibling files were checked beside it.
                    for &index in covered {
                        let mut copy = finding.clone();
                        if let Some(path) = finding.path.as_deref().filter(|p| !lookup.is_root(p, &unit.root)) {
                            copy.message = format!("{path}: {}", copy.message);
                        }
                        copy.path = None;
                        copy.line = None;
                        copy.column = None;
                        findings[index].push(copy);
                    }
                }
                (None, Findings::Own) if finding.path.is_none() => {
                    for &index in covered {
                        findings[index].push(finding.clone());
                    }
                }
                (None, Findings::Own) => {}
            }
        }
    }

    fn diagnostic(&self, finding: &Finding) -> Diagnostic {
        let mut lines = finding.message.lines();
        let title = lines.next().unwrap_or_default().trim().to_owned();
        let rest: Vec<&str> = lines.collect();
        let span = finding.line.map(|line| {
            let column = finding.column.unwrap_or(1);
            Span {
                start_line: line,
                start_col: column,
                end_line: finding.end_line.unwrap_or(line),
                end_col: finding.end_column.unwrap_or(column),
            }
        });
        let mut metadata = BTreeMap::new();
        if finding.fixable && self.can_fix() {
            metadata.insert(FIXABLE.to_owned(), "1".to_owned());
        }
        Diagnostic {
            engine: self.tool.name.clone(),
            code: finding.code.clone(),
            severity: finding.severity,
            title,
            description: (!rest.is_empty()).then(|| rest.join("\n")),
            span,
            url: finding.url.clone(),
            fix: Vec::new(),
            metadata,
        }
    }

    /// For files the tool says it can fix, run its fixer on a scratch copy and
    /// attach the result as a whole-file edit to the first fixable finding.
    fn attach_fixes(&self, members: &[Member<'_>], results: &mut [Option<anyhow::Result<Vec<Diagnostic>>>]) {
        if !self.can_fix() {
            return;
        }
        let fixable: Vec<usize> = (0..members.len())
            .filter(
                |&i| matches!(&results[i], Some(Ok(diags)) if diags.iter().any(|d| d.metadata.contains_key(FIXABLE))),
            )
            .collect();
        if fixable.is_empty() {
            return;
        }
        let rewritten = self.rewrite(
            &self.fix_passes,
            &fixable.iter().map(|&i| &members[i]).collect::<Vec<_>>(),
        );
        for (&index, outcome) in fixable.iter().zip(rewritten) {
            let Some(Ok(diags)) = &mut results[index] else { continue };
            if let Ok(Some(new)) = outcome
                && let Some(first) = diags.iter_mut().find(|d| d.metadata.contains_key(FIXABLE))
            {
                first.fix = vec![Edit {
                    start_byte: 0,
                    end_byte: members[index].src.content.len(),
                    replacement: new,
                }];
            }
            for diag in diags.iter_mut() {
                diag.metadata.remove(FIXABLE);
            }
        }
    }
}

/// Metadata marker set on a diagnostic the tool reported as auto-fixable, removed
/// once the fix is attached.
const FIXABLE: &str = "poly.fixable";

/// Whether a `./...`-style package pattern would skip this file: Go tooling
/// ignores directories named `testdata` or `vendor` and any starting with `.` or `_`.
/// A tool pointed at directories one by one must apply the same rule, or it lints
/// code the project never builds.
fn is_ignored_by_go_tooling(rel: &str) -> bool {
    let mut components = rel.split('/');
    components.next_back();
    components.any(|c| c == "testdata" || c == "vendor" || c.starts_with('.') || c.starts_with('_'))
}

/// `src/a/b.go` -> `./src/a`; a root-level file -> `.`.
fn directory_of(rel: &str) -> String {
    match rel.rsplit_once('/') {
        Some((dir, _)) => format!("./{dir}"),
        None => ".".to_owned(),
    }
}

/// Matching the paths a tool printed against the run's files.
struct Lookup {
    /// The ways the tree's root is spelled: as given and through symlinks
    /// (`/tmp` and `/private/tmp`).
    bases: Vec<PathBuf>,
}

impl Lookup {
    fn new(base: &Path) -> Self {
        let mut bases = vec![paths::normalize(base)];
        let real = paths::canonical(base);
        if !bases.contains(&real) {
            bases.push(real);
        }
        Lookup { bases }
    }

    /// Whether `raw` names the invocation root itself (helm prints the chart
    /// directory for chart-wide problems).
    fn is_root(&self, raw: &str, root: &Path) -> bool {
        let path = Path::new(raw);
        let joined = paths::normalize(&root.join(path));
        joined == paths::normalize(root) || paths::canonical(&joined) == paths::canonical(root)
    }

    /// Every spelling a member's path can be printed under.
    fn keys(&self, rel: &Path) -> Vec<PathBuf> {
        self.bases.iter().map(|base| base.join(rel)).collect()
    }

    /// The member `raw` names, trying the spellings tools use: absolute (possibly
    /// through a symlinked prefix), relative to the invocation root, and relative
    /// with a leading chart-name component.
    fn resolve(&self, raw: &str, root: &Path, hints: &[PathBuf], owners: &HashMap<PathBuf, usize>) -> Option<usize> {
        let path = Path::new(raw);
        let mut candidates: Vec<PathBuf> = Vec::new();
        if path.is_absolute() {
            candidates.push(paths::normalize(path));
        } else {
            candidates.push(paths::normalize(&root.join(path)));
            // Some tools print paths relative to where their config lives
            // (golangci-lint), not to where they ran.
            candidates.extend(self.bases.iter().map(|base| paths::normalize(&base.join(path))));
            for hint in hints {
                for base in &self.bases {
                    candidates.push(paths::normalize(&base.join(hint).join(path)));
                }
            }
            let mut components = path.components();
            if components.next().is_some() {
                candidates.push(paths::normalize(&root.join(components.as_path())));
            }
        }
        let real_root = paths::canonical(root);
        let found = candidates.into_iter().find_map(|candidate| {
            owners
                .get(&candidate)
                .or_else(|| owners.get(&paths::canonical(&candidate)))
                .or_else(|| {
                    let tail = candidate.strip_prefix(&real_root).ok()?;
                    owners.get(&paths::normalize(&root.join(tail)))
                })
                .copied()
        });
        found.or_else(|| Self::by_suffix(path, owners))
    }

    /// A path that climbs out of the invocation directory (`../../../x/y.tf`) is
    /// a tool relativizing against a different spelling of the working directory
    /// than poly's (a symlinked `/tmp`). What follows the climb is still the
    /// file's own path, so match on that tail.
    fn by_suffix(path: &Path, owners: &HashMap<PathBuf, usize>) -> Option<usize> {
        let tail: PathBuf = path.components().skip_while(|c| matches!(c, Component::ParentDir)).collect();
        if tail.as_os_str().is_empty() || tail == path {
            return None;
        }
        let mut matches = owners.iter().filter(|(key, _)| key.ends_with(&tail));
        let first = matches.next()?;
        matches.next().is_none().then_some(*first.1)
    }
}
