//! `poly migrate` — absorb a repo's foreign tool configs into `poly.toml`
//! (comment-preserving) and delete only the sources poly can fully honor.
//!
//! The default mode is a dry-run **report**; `--write` applies the plan. Each
//! importer (ruff / typos / taplo / markdownlint) yields `poly.toml` fragments
//! and an [`importers::Absorb`] verdict; the deletion policy decides,
//! per source, whether it may be removed, stripped (pyproject sections), or
//! kept. Merging into an existing `poly.toml` prefers keys already present, so
//! re-running `--write` is idempotent.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::Args;
use toml_edit::DocumentMut;

pub mod deletion;
pub mod importers;
pub mod report;

use deletion::Action;
use importers::{Absorb, ImportResult};

/// Directory names never descended into during `--recurse`.
const RECURSE_SKIP: &[&str] = &[".git", "node_modules", "target", ".polylint", "vendor", "dist", "build"];

/// Source filenames that mark a directory as a migration target under `--recurse`.
const MIGRATABLE_SOURCES: &[&str] = &[
    "ruff.toml",
    ".ruff.toml",
    "pyproject.toml",
    "_typos.toml",
    ".typos.toml",
    "typos.toml",
    ".taplo.toml",
    "taplo.toml",
    ".markdownlint.json",
    ".markdownlint.jsonc",
    ".markdownlint.yaml",
    ".markdownlint.yml",
];

/// `poly migrate` arguments.
#[derive(Args)]
pub struct MigrateArgs {
    /// Repository directory to migrate (default: current directory).
    pub path: Option<PathBuf>,

    /// Apply the plan: write `poly.toml`, delete/strip absorbed sources. The
    /// default is a dry-run report that writes nothing.
    #[arg(long)]
    pub write: bool,

    /// Explicit dry-run report (the default): print the plan, write nothing.
    #[arg(long, conflicts_with = "write")]
    pub report: bool,

    /// Recurse into nested project directories (monorepo), skipping `.git`,
    /// `node_modules`, `target`, and other vendored/build trees.
    #[arg(long)]
    pub recurse: bool,

    /// After `--write`, run `poly lint` / `poly fmt --check` to confirm the new
    /// config loads and every engine runs without error.
    #[arg(long)]
    pub verify: bool,

    /// Allow `--write` even when the git working tree is dirty.
    #[arg(long)]
    pub allow_dirty: bool,

    /// Also strip `pyproject.toml` `[tool.<x>]` sections for Python formatters
    /// and linters superseded by ruff (black, isort, flake8, pyproject-fmt, …).
    /// Off by default; poly does not run these tools, so keeping their config is
    /// harmless — this is an opt-in cleanup for repos consolidating onto poly.
    #[arg(long)]
    pub strip_superseded: bool,
}

/// `pyproject.toml` `[tool.<x>]` sections whose tool is superseded by ruff (or
/// poly's opinionated formatting) and which `--strip-superseded` removes.
const SUPERSEDED_PYPROJECT_TOOLS: &[&str] = &[
    "black",
    "isort",
    "flake8",
    "pyproject-fmt",
    "blacken-docs",
    "autoflake",
    "yapf",
    "pycln",
    "docformatter",
    "autopep8",
];

/// A migration plan for a single directory.
pub struct MigrationPlan {
    /// The directory being migrated.
    pub dir: PathBuf,
    /// The `poly.toml` this plan targets.
    pub poly_toml_path: PathBuf,
    /// Whether that `poly.toml` already existed.
    pub existed: bool,
    /// The merged document (existing content plus absorbed fragments).
    pub doc: DocumentMut,
    /// Per-importer results.
    pub results: Vec<ImportResult>,
    /// Deletion actions for absorbed sources.
    pub actions: Vec<Action>,
    /// KEEP / REPORT-ONLY actions for delegated and publisher files.
    pub kept: Vec<Action>,
    /// Conflict notes (existing keys left untouched).
    pub conflicts: Vec<String>,
}

impl MigrationPlan {
    /// Whether the plan would change anything on disk.
    pub fn is_empty(&self) -> bool {
        self.results.iter().all(|r| !r.has_fragments())
            && !self
                .actions
                .iter()
                .any(|a| !matches!(a, Action::Keep { .. } | Action::ReportOnly { .. }))
    }
}

/// Entry point for `poly migrate`.
pub fn run_migrate(args: MigrateArgs) -> ExitCode {
    match run(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("poly migrate: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run(args: MigrateArgs) -> Result<ExitCode> {
    let root = args.path.clone().unwrap_or_else(|| PathBuf::from("."));
    if !root.is_dir() {
        bail!("not a directory: {}", root.display());
    }
    let dirs = if args.recurse {
        discover_dirs(&root)
    } else {
        vec![root.clone()]
    };

    if args.write && !args.allow_dirty {
        guard_clean_tree(&root)?;
    }

    let mut any_change = false;
    for dir in &dirs {
        let mut plan = build_plan(dir)?;
        if args.strip_superseded {
            plan.actions.extend(superseded_actions(dir));
        }
        if plan.results.iter().any(|r| r.absorb != Absorb::None) || !plan.kept.is_empty() || !plan.actions.is_empty() {
            print!("{}", report::render_plan(&plan, args.write));
        }
        if args.write && !plan.is_empty() {
            apply_plan(&plan).with_context(|| format!("applying migration in {}", dir.display()))?;
            any_change = true;
            if args.verify {
                verify(dir)?;
            }
        }
    }

    if args.write && !any_change {
        println!("Nothing to migrate.");
    }
    Ok(ExitCode::SUCCESS)
}

/// Build the migration plan for one directory: run every importer, merge the
/// fragments into the directory's `poly.toml`, and compute the file decisions.
pub fn build_plan(dir: &Path) -> Result<MigrationPlan> {
    let poly_toml_path = dir.join("poly.toml");
    let existed = poly_toml_path.is_file();
    let mut doc = if existed {
        let text = std::fs::read_to_string(&poly_toml_path)
            .with_context(|| format!("reading {}", poly_toml_path.display()))?;
        text.parse::<DocumentMut>()
            .with_context(|| format!("parsing {}", poly_toml_path.display()))?
    } else {
        DocumentMut::new()
    };

    let importers: [fn(&Path) -> Option<ImportResult>; 4] = [
        importers::ruff::import,
        importers::typos::import,
        importers::taplo::import,
        importers::markdownlint::import,
    ];
    let mut results: Vec<ImportResult> = Vec::new();
    for import in importers {
        if let Some(result) = import(dir) {
            results.push(result);
        }
    }

    let mut conflicts = Vec::new();
    for result in &results {
        conflicts.extend(importers::apply(&mut doc, &result.fragments));
    }

    let actions = deletion::plan_actions(&results);
    let kept = deletion::scan_kept(dir);

    Ok(MigrationPlan {
        dir: dir.to_path_buf(),
        poly_toml_path,
        existed,
        doc,
        results,
        actions,
        kept,
        conflicts,
    })
}

/// Apply a plan: write `poly.toml`, then delete/strip absorbed sources.
fn apply_plan(plan: &MigrationPlan) -> Result<()> {
    std::fs::write(&plan.poly_toml_path, plan.doc.to_string())
        .with_context(|| format!("writing {}", plan.poly_toml_path.display()))?;
    for action in &plan.actions {
        match action {
            Action::DeleteFile(path) => {
                std::fs::remove_file(path).with_context(|| format!("deleting {}", path.display()))?;
            }
            Action::StripPyproject { path, sections } => {
                strip_pyproject(path, sections)?;
            }
            Action::Keep { .. } | Action::ReportOnly { .. } => {}
        }
    }
    Ok(())
}

/// Build a `StripPyproject` action for any superseded `[tool.<x>]` sections
/// present in `dir/pyproject.toml` (empty when none / no pyproject).
fn superseded_actions(dir: &Path) -> Vec<Action> {
    let path = dir.join("pyproject.toml");
    let Some(table) = importers::load_toml(&path) else {
        return Vec::new();
    };
    let Some(tool) = table.get("tool").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    let sections: Vec<Vec<String>> = SUPERSEDED_PYPROJECT_TOOLS
        .iter()
        .filter(|name| tool.contains_key(**name))
        .map(|name| vec!["tool".to_string(), (*name).to_string()])
        .collect();
    if sections.is_empty() {
        Vec::new()
    } else {
        vec![Action::StripPyproject { path, sections }]
    }
}

/// Remove the given dotted sections from a `pyproject.toml`, never deleting the
/// file itself. Empty parent tables (e.g. a now-empty `[tool]`) are left as-is.
fn strip_pyproject(path: &Path, sections: &[Vec<String>]) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc = text
        .parse::<DocumentMut>()
        .with_context(|| format!("parsing {}", path.display()))?;
    for section in sections {
        remove_dotted(doc.as_table_mut(), section);
    }
    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Remove the leaf named by `path` from the table tree rooted at `table`.
fn remove_dotted(table: &mut toml_edit::Table, path: &[String]) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut current = table;
    for parent in parents {
        match current.get_mut(parent).and_then(|item| item.as_table_mut()) {
            Some(next) => current = next,
            None => return,
        }
    }
    current.remove(last);
}

/// Refuse to write when the git working tree under `dir` has uncommitted
/// changes. A non-git directory (or missing `git`) is treated as clean.
fn guard_clean_tree(dir: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain"])
        .output();
    match output {
        Ok(out) if out.status.success() && !out.stdout.is_empty() => {
            bail!("git working tree is dirty; commit or stash first, or pass --allow-dirty to override");
        }
        _ => Ok(()),
    }
}

/// Discover migration-target directories under `root` for `--recurse`: any
/// directory holding a migratable source, skipping vendored/build trees.
fn discover_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let walker = walkdir::WalkDir::new(root).into_iter().filter_entry(|entry| {
        if !entry.file_type().is_dir() {
            return true;
        }
        // Never descend into the root itself being skipped; only skip nested.
        entry.depth() == 0
            || !entry
                .file_name()
                .to_str()
                .is_some_and(|name| RECURSE_SKIP.contains(&name))
    });
    for entry in walker.flatten() {
        if entry.file_type().is_dir() && dir_has_source(entry.path()) {
            dirs.push(entry.path().to_path_buf());
        }
    }
    dirs
}

/// Whether `dir` directly contains any migratable source file.
fn dir_has_source(dir: &Path) -> bool {
    MIGRATABLE_SOURCES.iter().any(|name| dir.join(name).is_file())
}

/// Opt-in verification: load the freshly written config and run lint + format
/// (dry-run) to confirm every engine executes without error.
fn verify(dir: &Path) -> Result<()> {
    use poly_core::{Config, RunOptions};
    let config = Config::load(dir).with_context(|| format!("loading config in {}", dir.display()))?;
    let options = RunOptions {
        no_cache: true,
        jobs: None,
        exclude: Vec::new(),
        explicit_config: false,
    };
    let paths = [dir.to_path_buf()];
    poly_core::lint(&paths, &config, &options, false, false).context("verify: poly lint failed")?;
    poly_core::format(&paths, &config, &options, false, false).context("verify: poly fmt --check failed")?;
    println!("verify: config loaded and engines ran cleanly in {}", dir.display());
    Ok(())
}

#[cfg(test)]
mod superseded_tests {
    use super::*;

    #[test]
    fn strips_present_superseded_tables_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.black]\nline-length = 88\n[tool.isort]\nprofile = \"black\"\n[tool.mypy]\nstrict = true\n",
        )
        .unwrap();
        let actions = superseded_actions(dir.path());
        let Action::StripPyproject { sections, .. } = actions.first().expect("one strip action") else {
            panic!("expected StripPyproject");
        };
        assert!(sections.contains(&vec!["tool".to_string(), "black".to_string()]));
        assert!(sections.contains(&vec!["tool".to_string(), "isort".to_string()]));
        // mypy is kept (type checker, not superseded).
        assert!(!sections.contains(&vec!["tool".to_string(), "mypy".to_string()]));
    }

    #[test]
    fn no_superseded_tables_yields_no_action() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.mypy]\nstrict = true\n").unwrap();
        assert!(superseded_actions(dir.path()).is_empty());
    }
}
