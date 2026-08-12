//! File discovery: walk the given paths respecting `.gitignore`, and tag each
//! file with its detected [`Language`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ignore::WalkBuilder;
use ignore::overrides::{Override, OverrideBuilder};
use serde::Serialize;

use crate::language::Language;
use crate::resolve::ConfigSet;

/// Directory names pruned from every walk regardless of git-tracking.
///
/// These hold vendored third-party code (`node_modules`, `vendor`, `deps`),
/// build artifacts (`target`, `dist`, `build`, `.next`, `.nuxt`, `.gradle`),
/// or tool caches/environments (`.venv`, `venv`, `__pycache__`, `.mypy_cache`,
/// `.ruff_cache`, `.pytest_cache`, `.tox`, `coverage`, `.polylint`, `.git`).
/// None of it is source the user authored, so no linter or formatter should
/// touch it — and these directories are frequently *tracked* in git (e.g.
/// committed Hex `deps/` CHANGELOGs), so `.gitignore` alone does not exclude
/// them. Pruning them at the walk boundary also avoids descending into large
/// generated subtrees.
const PRUNED_DIRECTORIES: &[&str] = &[
    "node_modules",
    "vendor",
    "deps",
    "target",
    "dist",
    "build",
    ".git",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".tox",
    ".gradle",
    ".next",
    ".nuxt",
    "coverage",
    ".polylint",
];

/// A file found during discovery, tagged with its detected language and the id
/// of the config (within the run's [`ConfigSet`]) that governs it.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    /// Path to the file.
    pub path: PathBuf,
    /// Detected language.
    pub language: Language,
    /// Index into the run's [`ConfigSet`] of the nearest config governing this
    /// file (`0` = the run's root config). Set by [`discover`].
    pub config_id: usize,
}

/// How many distinct depths' worth of sample directories one rule keeps.
///
/// The samples exist to *show* a reader where a rule reached, so one per depth
/// is the useful unit: a rule pruning five hundred directories all at depth four
/// is doing one thing, while a rule pruning two at depths one and seven is
/// almost certainly doing something its author did not intend. The cap bounds
/// the memory a pathological glob can cost; no real exclude nests further.
const MAX_SAMPLED_DEPTHS: usize = 8;

/// One directory an exclude rule pruned, kept as evidence of where the rule
/// actually reaches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ExcludedDirectory {
    /// The directory, spelled the way the walk saw it (relative to the walk root
    /// when the root itself was relative).
    pub path: PathBuf,
    /// Path components between the walk root and this directory. A top-level
    /// `e2e/` is depth 1.
    pub depth: usize,
}

/// One `[discovery] exclude` / `--exclude` glob together with what it actually
/// pruned during a run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ExcludedRule {
    /// The glob exactly as written in config (or passed to `--exclude`).
    pub pattern: String,
    /// Files this rule pruned. Exact — each was seen individually by the walk.
    pub files: usize,
    /// Directories this rule pruned. The walk never descended into them, so the
    /// files they contain are counted nowhere.
    pub directories: usize,
    /// The first directory this rule pruned at each distinct depth, capped at
    /// [`MAX_SAMPLED_DEPTHS`].
    ///
    /// An exclude glob with no `/` in it — including the bare directory name
    /// poly derives from a `foo/**` pattern — matches at *any* depth, so a rule
    /// meant for one top-level directory can quietly prune a same-named package
    /// deep in a source tree. Recording one directory per depth is what lets
    /// `poly doctor` name that directory instead of merely suspecting it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directory_samples: Vec<ExcludedDirectory>,
}

impl ExcludedRule {
    /// How many distinct depths this rule pruned directories at (saturating at
    /// [`MAX_SAMPLED_DEPTHS`]).
    ///
    /// More than one is the signal that a pattern is broader than the single
    /// directory its author was picturing.
    pub fn distinct_directory_depths(&self) -> usize {
        self.directory_samples.len()
    }
}

/// What `[discovery] exclude` / `--exclude` removed from a run.
///
/// This exists so a summary can distinguish "everything is clean" from
/// "everything I chose to look at is clean". The distinction is not cosmetic: a
/// repo excluding `test_apps/**` carried formatting drift for weeks because the
/// top-level check reported an unqualified pass.
///
/// ## Why there is no single "N files excluded" number
///
/// The walker *prunes* an excluded directory — it is never descended into, which
/// is the entire reason an exclude is cheap. Producing an exact excluded-file
/// count would mean walking the very trees the exclude exists to avoid, on every
/// run, for a line of summary text. So this records only what the walk genuinely
/// observed: the entries it pruned, with files and directories kept apart, and
/// the rule each is attributable to. [`excluded_files`](Self::excluded_files) is
/// exact; the contents of the [`excluded_directories`](Self::excluded_directories)
/// are deliberately unmeasured, and the renderers say so rather than implying a
/// precision that was never paid for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DiscoveryReport {
    /// Individual files pruned by an exclude rule. Exact.
    pub excluded_files: usize,
    /// Directories pruned by an exclude rule. Not descended into, so their
    /// contents are not reflected in `excluded_files`.
    pub excluded_directories: usize,
    /// How many of `excluded_files` were named explicitly by the caller and
    /// dropped by `--force-exclude`. This is the case where a run can check
    /// nothing at all and still exit green, so it is called out separately.
    pub excluded_explicit: usize,
    /// Per-rule attribution, most-pruning first. Rules that matched nothing are
    /// omitted.
    pub rules: Vec<ExcludedRule>,
}

impl DiscoveryReport {
    /// Whether discovery pruned nothing at all — the common case, where no
    /// qualification needs to be added to a summary.
    pub fn is_empty(&self) -> bool {
        self.excluded_files == 0 && self.excluded_directories == 0
    }

    /// Fold one rule's tally into this report, accumulating across walk roots.
    fn record_rule(&mut self, pattern: &str, hits: &RuleHits) {
        if hits.files == 0 && hits.directories == 0 {
            return;
        }
        match self.rules.iter_mut().find(|r| r.pattern == pattern) {
            Some(rule) => {
                rule.files += hits.files;
                rule.directories += hits.directories;
                for sample in &hits.samples {
                    push_sample(&mut rule.directory_samples, sample);
                }
            }
            None => self.rules.push(ExcludedRule {
                pattern: pattern.to_owned(),
                files: hits.files,
                directories: hits.directories,
                directory_samples: hits.samples.clone(),
            }),
        }
    }

    /// Order rules by how much they pruned (then by pattern, for determinism),
    /// and each rule's sampled directories shallowest-first — so a reader sees
    /// the directory the rule was probably written for before the ones it
    /// caught by accident.
    fn sort_rules(&mut self) {
        self.rules.sort_by(|a, b| {
            (b.files + b.directories)
                .cmp(&(a.files + a.directories))
                .then_with(|| a.pattern.cmp(&b.pattern))
        });
        for rule in &mut self.rules {
            rule.directory_samples.sort_by_key(|sample| sample.depth);
        }
    }
}

/// Whether a directory at `depth` would be retained as a sample.
///
/// Checked *before* building an [`ExcludedDirectory`] on the walk path: a single
/// walk can prune thousands of directories (one `node_modules` rule is enough),
/// but only the first at each of [`MAX_SAMPLED_DEPTHS`] depths is ever kept.
/// Asking first keeps the discarded majority free of a `PathBuf` allocation.
fn wants_sample(samples: &[ExcludedDirectory], depth: usize) -> bool {
    samples.len() < MAX_SAMPLED_DEPTHS && !samples.iter().any(|s| s.depth == depth)
}

/// Keep `sample` if this is the first directory seen at its depth and there is
/// room left under [`MAX_SAMPLED_DEPTHS`].
fn push_sample(samples: &mut Vec<ExcludedDirectory>, sample: &ExcludedDirectory) {
    if !wants_sample(samples, sample.depth) {
        return;
    }
    samples.push(sample.clone());
}

/// What one exclude rule pruned during one walk.
#[derive(Debug, Default, Clone)]
struct RuleHits {
    files: usize,
    directories: usize,
    /// One pruned directory per distinct depth, capped at [`MAX_SAMPLED_DEPTHS`].
    samples: Vec<ExcludedDirectory>,
}

/// Running tally of what one walk root's exclude set pruned.
#[derive(Debug, Default)]
struct ExcludeHits {
    files: usize,
    directories: usize,
    explicit: usize,
    /// Per-rule tallies, positionally parallel to [`ExcludeMatcher::per_rule`].
    per_rule: Vec<RuleHits>,
}

/// The compiled exclude set for one walk root, plus the tally of what it pruned.
///
/// `combined` is the matcher that decides — one globset pass per walked entry,
/// exactly the work [`ignore`] would have done internally had the set been handed
/// to `WalkBuilder::overrides`, so counting the prunes costs nothing per entry.
///
/// Attribution is the part that is *not* free: naming the rule responsible needs
/// one compiled matcher per glob, and compiling a glob set is far from free
/// (measured at ~0.5 ms for a 17-glob exclude list, ~20% of a whole discovery
/// pass over this repo). So `per_rule` is built lazily, on the first entry that
/// is actually excluded: a run whose excludes never fire — and every run with no
/// excludes at all — pays nothing, and a run that does exclude something pays
/// once, for the answer it is about to print.
#[derive(Debug)]
struct ExcludeMatcher {
    root: PathBuf,
    patterns: Vec<String>,
    combined: Override,
    /// One matcher per entry of `patterns`, positionally aligned; `None` for a
    /// glob that failed to compile. Built on first exclusion.
    per_rule: std::sync::OnceLock<Vec<Option<Override>>>,
    hits: Mutex<ExcludeHits>,
}

impl ExcludeMatcher {
    /// Compile `exclude` relative to `root`. `None` when there is nothing to
    /// exclude, which lets the walk skip matching entirely.
    fn new(root: &Path, exclude: &[String]) -> Option<Arc<Self>> {
        let combined = build_excludes(root, exclude)?;
        Some(Arc::new(Self {
            root: root.to_path_buf(),
            patterns: exclude.to_vec(),
            combined,
            per_rule: std::sync::OnceLock::new(),
            hits: Mutex::new(ExcludeHits::default()),
        }))
    }

    /// The per-rule matchers, compiled on first use.
    fn per_rule(&self) -> &[Option<Override>] {
        self.per_rule.get_or_init(|| {
            self.patterns
                .iter()
                .map(|glob| build_excludes(&self.root, std::slice::from_ref(glob)))
                .collect()
        })
    }

    /// Whether `path` is excluded; when it is, tally it (and the rule that
    /// matched) before returning `true`. `explicit` marks a path the caller
    /// named directly, dropped under `--force-exclude`.
    fn record_if_excluded(&self, path: &Path, is_dir: bool, explicit: bool) -> bool {
        if !self.combined.matched(path, is_dir).is_ignore() {
            return false;
        }
        let attributed = self
            .per_rule()
            .iter()
            .position(|matcher| matcher.as_ref().is_some_and(|m| m.matched(path, is_dir).is_ignore()));
        let mut hits = self.hits.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if is_dir {
            hits.directories += 1;
        } else {
            hits.files += 1;
        }
        if explicit {
            hits.explicit += 1;
        }
        if let Some(index) = attributed {
            if hits.per_rule.is_empty() {
                hits.per_rule = vec![RuleHits::default(); self.patterns.len()];
            }
            let rule = &mut hits.per_rule[index];
            if is_dir {
                rule.directories += 1;
                let depth = self.depth_of(path);
                if wants_sample(&rule.samples, depth) {
                    rule.samples.push(ExcludedDirectory {
                        path: path.to_path_buf(),
                        depth,
                    });
                }
            } else {
                rule.files += 1;
            }
        }
        true
    }

    /// How many path components separate `path` from the walk root. A directory
    /// sitting directly in the root is depth 1. A path outside the root (the
    /// `--force-exclude` explicit case, matched from the working directory)
    /// falls back to its own component count, which keeps the number monotonic
    /// rather than accurate — it is only ever compared against other depths from
    /// the same matcher.
    fn depth_of(&self, path: &Path) -> usize {
        path.strip_prefix(&self.root)
            .map(|relative| relative.components().count())
            .unwrap_or_else(|_| path.components().count())
    }

    /// Fold this root's tally into the run-wide report.
    fn merge_into(&self, report: &mut DiscoveryReport) {
        let hits = self.hits.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        report.excluded_files += hits.files;
        report.excluded_directories += hits.directories;
        report.excluded_explicit += hits.explicit;
        for (pattern, rule) in self.patterns.iter().zip(&hits.per_rule) {
            report.record_rule(pattern, rule);
        }
    }
}

/// `filter_entry` predicate shared by discovery and config scanning: prune the
/// vendored/generated directories in [`PRUNED_DIRECTORIES`], but only when they
/// are nested (`depth > 0`) and are directories — so an explicitly passed root
/// such as `node_modules/foo.js` is still walked, and a plain file that happens
/// to share one of these names is never dropped. Returns `true` to keep.
pub(crate) fn keep_walk_entry(entry: &ignore::DirEntry) -> bool {
    let is_directory = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
    if entry.depth() > 0 && is_directory {
        let name = entry.file_name();
        return !PRUNED_DIRECTORIES.iter().any(|pruned| name == *pruned);
    }
    true
}

/// Detect a file's language: tier-1 extension mapping first, then the
/// tree-sitter language pack's path detection for the long tail (mapped to
/// [`Language::Other`] so the generic tier handles it). Files neither can
/// identify are skipped.
fn detect(path: &std::path::Path) -> Option<Language> {
    if let Some(language) = Language::from_path(path) {
        return Some(language);
    }
    let name = tree_sitter_language_pack::detect_language(&path.to_string_lossy())?;
    Some(Language::Other(name.to_string()))
}

/// Recursively discover supported files under `paths`, tagging each with the
/// nearest config in `configs` that governs it (ADR 0018).
///
/// For each walk root, the exclude set is the union of every in-tree config's
/// `[discovery] exclude` (each rooted at its own config directory via
/// [`ConfigSet::walk_excludes`]) plus the call-time `extra` globs. Matching paths
/// are pruned from the walk in addition to `.gitignore` and the built-in
/// `PRUNED_DIRECTORIES` set.
pub fn discover(paths: &[PathBuf], configs: &ConfigSet, extra: &[String]) -> Vec<DiscoveredFile> {
    discover_with(paths, configs, extra, false)
}

/// [`discover`], with control over whether excludes also apply to explicitly
/// named files.
///
/// A directory walk prunes excluded paths, but a file named directly on the
/// command line was always kept — reasonable when a human names a file, wrong in
/// a hook, which is *always* handed explicit staged paths and never names
/// anything deliberately. A repo excluding `**/*.tf` from discovery therefore
/// found the exclude silently inert in its pre-commit hook, and poly reformatted
/// Terraform that `terraform fmt` then rejected.
///
/// `force_exclude` applies the exclude set to explicit paths too. It is on for
/// the hook path and off for a direct CLI invocation, matching what ruff and
/// black settled on for the same reason.
pub fn discover_with(
    paths: &[PathBuf],
    configs: &ConfigSet,
    extra: &[String],
    force_exclude: bool,
) -> Vec<DiscoveredFile> {
    discover_reporting(paths, configs, extra, force_exclude).0
}

/// [`discover_with`], additionally returning the [`DiscoveryReport`] describing
/// what the exclude set pruned.
///
/// The exclude globs are matched here in `filter_entry` rather than handed to
/// `WalkBuilder::overrides`, because [`ignore`] applies its own matchers *before*
/// the user predicate and never surfaces the entries it drops — the excluded
/// paths would be invisible. Matching them ourselves costs the same single
/// globset pass per entry that `overrides` performed internally, and pruning
/// still happens at the directory boundary, so an excluded tree is no more
/// walked than before.
pub fn discover_reporting(
    paths: &[PathBuf],
    configs: &ConfigSet,
    extra: &[String],
    force_exclude: bool,
) -> (Vec<DiscoveredFile>, DiscoveryReport) {
    let mut out = Vec::new();
    let mut report = DiscoveryReport::default();
    for root in paths {
        let exclude = configs.walk_excludes(root, extra);
        if force_exclude
            && root.is_file()
            && let Some(matcher) = explicit_matcher(&exclude)
            && matcher.record_if_excluded(root, false, true)
        {
            matcher.merge_into(&mut report);
            continue;
        }
        let matcher = ExcludeMatcher::new(root, &exclude);
        let walk_matcher = matcher.clone();
        let mut builder = WalkBuilder::new(root);
        builder
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .parents(true)
            .filter_entry(move |entry| {
                if !keep_walk_entry(entry) {
                    return false;
                }
                match &walk_matcher {
                    Some(matcher) => {
                        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        !matcher.record_if_excluded(entry.path(), is_dir, false)
                    }
                    None => true,
                }
            });
        let walker = builder.build();
        for entry in walker.flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            if let Some(language) = detect(path) {
                out.push(DiscoveredFile {
                    path: path.to_path_buf(),
                    language,
                    config_id: configs.config_id_for(path),
                });
            }
        }
        if let Some(matcher) = &matcher {
            matcher.merge_into(&mut report);
        }
    }
    report.sort_rules();
    (out, report)
}

/// The matcher used for an explicitly named file under `--force-exclude`.
///
/// The globs are written relative to the config's directory, so they are matched
/// from the current directory — the same frame of reference a user writing
/// `terraform/**` in `poly.toml` at the repo root is using. A file outside that
/// frame simply does not match, which is the safe direction: it gets checked.
fn explicit_matcher(exclude: &[String]) -> Option<Arc<ExcludeMatcher>> {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    ExcludeMatcher::new(&base, exclude)
}

/// Build an [`ignore::overrides::Override`] from `[discovery] exclude` globs,
/// rooted at `root`. Each glob is added with a leading `!` so it acts as an
/// ignore (exclusion) — with no whitelist glob present, the override behaves
/// like a `.gitignore`: matched paths are pruned and everything else is kept.
/// Returns `None` when there is nothing to exclude. An individual malformed
/// glob is skipped with a warning rather than aborting discovery.
///
/// ## Anchoring
///
/// Gitignore rules apply verbatim: a glob with no `/` matches a file or
/// directory of that name at **any** depth, while a leading `/` anchors it to
/// `root`. The `foo/**` shape also gets a derived bare `foo` rule, without which
/// the directory itself is never pruned (only its contents) and the walk
/// descends into a tree the user asked to skip — but that derived rule carries
/// no separator, so `e2e/**` prunes every directory named `e2e` at every depth.
/// That is surprising when `e2e` is also a Java/Kotlin package name, and it is
/// why [`ExcludedRule::directory_samples`] records where a rule actually
/// reached: `poly doctor` reports any rule pruning at more than one depth.
/// Anchoring (`/e2e/**`) is the fix, and is deliberately *not* applied
/// automatically — silently re-interpreting an existing exclude would change
/// which files a repo's gate covers without anyone editing anything.
fn build_excludes(root: &std::path::Path, exclude: &[String]) -> Option<ignore::overrides::Override> {
    if exclude.is_empty() {
        return None;
    }
    let mut builder = OverrideBuilder::new(root);
    for glob in exclude {
        if let Err(error) = builder.add(&format!("!{glob}")) {
            tracing::warn!(%glob, %error, "skipping invalid [discovery] exclude glob");
            continue;
        }
        if let Some(dir) = glob.strip_suffix("/**")
            && let Err(error) = builder.add(&format!("!{dir}"))
        {
            tracing::warn!(%dir, %error, "skipping derived directory exclude");
        }
    }
    match builder.build() {
        Ok(overrides) => Some(overrides),
        Err(error) => {
            tracing::warn!(%error, "failed to build [discovery] exclude globs; ignoring them");
            None
        }
    }
}
