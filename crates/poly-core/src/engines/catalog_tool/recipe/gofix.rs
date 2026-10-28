//! golangci-lint's fixes, applied to a scratch copy of the Go module.
//!
//! `golangci-lint run --fix` judges a package with its siblings and its
//! `go.mod`, so the one-file mirror of [`RecipeTool::rewrite`] cannot compile
//! the code it is asked to fix. This path copies the whole module (never a
//! nested one, never anything outside it) into a scratch tree, lays poly's
//! bytes over the files of the run, runs the fix on the run's package
//! directories, and reads back only the files of the run. The real tree is not
//! touched, so lint mode stays read-only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use poly_catalog::Group;

use super::argv::Template;
use super::lint::{Member, directory_of, is_ignored_by_go_tooling};
use super::mirror::Mirror;
use super::{RecipeTool, exec, paths};

/// Directories never copied into the scratch module.
const SKIPPED_DIRS: &[&str] = &[".git", "node_modules"];

/// Where the scratch tree is rooted. With `new_from_rev` the tool asks git which
/// lines are new, so the tree is laid out like the repository: `base` is the
/// repository root, `prefix` the working directory below it, `git_dir` the
/// repository's own directory.
struct Layout {
    base: PathBuf,
    prefix: PathBuf,
    git_dir: Option<PathBuf>,
}

impl RecipeTool {
    /// Run `passes` over a scratch copy of each member's Go module and return
    /// each member's new content, or `None` where it came out unchanged.
    pub(super) fn rewrite_modules(
        &self,
        passes: &[Template],
        members: &[&Member<'_>],
    ) -> Vec<anyhow::Result<Option<String>>> {
        let mut outcomes: Vec<Option<anyhow::Result<Option<String>>>> = members.iter().map(|_| None).collect();
        let layout = self.layout();
        let mut modules: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
        for (index, member) in members.iter().enumerate() {
            let abs = paths::absolute(&self.cwd, &member.rel);
            if let Some(root) = self.module_root(&abs) {
                modules.entry(root).or_default().push(index);
            }
        }
        for (root, indices) in modules {
            match self.fix_module(passes, &layout, &root, members, &indices) {
                Ok(done) => {
                    for (index, text) in done {
                        outcomes[index] = Some(Ok(text));
                    }
                }
                Err(error) => {
                    for index in indices {
                        outcomes[index] = Some(Err(anyhow!("{error:#}")));
                    }
                }
            }
        }
        outcomes.into_iter().map(|o| o.unwrap_or(Ok(None))).collect()
    }

    fn module_root(&self, abs: &Path) -> Option<PathBuf> {
        match &self.recipe.group {
            Group::Marker(markers) => paths::project_root(abs, markers),
            _ => None,
        }
    }

    fn layout(&self) -> Layout {
        let plain = Layout {
            base: self.cwd.clone(),
            prefix: PathBuf::new(),
            git_dir: None,
        };
        if !self.scoped_to_rev {
            return plain;
        }
        let Some(output) = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.cwd)
            .args(["rev-parse", "--show-toplevel", "--absolute-git-dir"])
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .ok()
            .filter(|output| output.status.success())
        else {
            return plain;
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = text.lines();
        let (Some(top), Some(git_dir)) = (lines.next(), lines.next()) else {
            return plain;
        };
        let top = PathBuf::from(top);
        let Ok(prefix) = paths::canonical(&self.cwd).strip_prefix(&top).map(Path::to_path_buf) else {
            return plain;
        };
        Layout {
            base: top,
            prefix,
            git_dir: Some(PathBuf::from(git_dir)),
        }
    }

    fn fix_module(
        &self,
        passes: &[Template],
        layout: &Layout,
        root: &Path,
        members: &[&Member<'_>],
        indices: &[usize],
    ) -> anyhow::Result<Vec<(usize, Option<String>)>> {
        let mut dirs: Vec<String> = indices
            .iter()
            .filter_map(|&index| {
                let abs = paths::absolute(&self.cwd, &members[index].rel);
                let rel = paths::slashed(abs.strip_prefix(root).ok()?);
                (!is_ignored_by_go_tooling(&rel)).then(|| directory_of(&rel))
            })
            .collect();
        dirs.sort();
        dirs.dedup();
        if dirs.is_empty() {
            return Ok(Vec::new());
        }

        let mirror = Mirror::new()?;
        let module = root.strip_prefix(&self.cwd).context("the module is outside the working directory")?;
        let module_dir = mirror.root().join(&layout.prefix).join(module);
        copy_module(root, &module_dir)?;
        for &index in indices {
            let placed = layout.prefix.join(&members[index].rel);
            mirror.write(&placed, &members[index].src.content)?;
            mirror.copy_context(&layout.base, &placed, &self.context_names);
        }

        let mut env = self.env.clone();
        if let Some(git_dir) = &layout.git_dir {
            env.insert("GIT_DIR".to_owned(), git_dir.to_string_lossy().into_owned());
            env.insert("GIT_WORK_TREE".to_owned(), mirror.root().to_string_lossy().into_owned());
            env.insert("GIT_OPTIONAL_LOCKS".to_owned(), "0".to_owned());
        }
        let systems: Vec<Option<&String>> = if self.goos.is_empty() {
            vec![None]
        } else {
            self.goos.iter().map(Some).collect()
        };
        for goos in systems {
            let mut env = env.clone();
            if let Some(goos) = goos {
                env.insert("GOOS".to_owned(), goos.clone());
            }
            for pass in passes {
                for chunk in pass.chunks(&dirs) {
                    let argv = pass.render(&dirs[chunk]);
                    let captured = exec::run(&self.launcher, &argv, &module_dir, &env, self.unset_env)?;
                    if !captured.exited_ok(&self.recipe.ok_exits) {
                        return Err(anyhow!("{} failed: {}", self.tool.name, captured.explanation()));
                    }
                }
            }
        }

        indices
            .iter()
            .map(|&index| {
                let member = members[index];
                let path = mirror.root().join(layout.prefix.join(&member.rel));
                let bytes = std::fs::read(&path).with_context(|| format!("reading back {}", path.display()))?;
                let text = String::from_utf8(bytes)
                    .with_context(|| format!("{} is not UTF-8 after fixing", path.display()))?;
                Ok::<_, anyhow::Error>((index, (text != *member.src.content).then_some(text)))
            })
            .collect()
    }
}

/// Copy the regular files of the module at `source` into `target`, leaving out
/// nested modules (their own `go.mod`), symlinks and [`SKIPPED_DIRS`].
fn copy_module(source: &Path, target: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(target).with_context(|| format!("creating {}", target.display()))?;
    for entry in std::fs::read_dir(source).with_context(|| format!("reading {}", source.display()))? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let name = entry.file_name();
        let destination = target.join(&name);
        if kind.is_dir() {
            let skipped = SKIPPED_DIRS.iter().any(|skipped| name == *skipped);
            if !skipped && !entry.path().join("go.mod").is_file() {
                copy_module(&entry.path(), &destination)?;
            }
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &destination)
                .with_context(|| format!("copying {}", entry.path().display()))?;
        }
    }
    Ok(())
}
