//! The ruff configs a sparse snapshot's configs extend.
//!
//! A ruff config may `extend` a file anywhere in the repository
//! (`extend = "../../shared/ruff-base.toml"`). The sparse selection holds the
//! staged files' directory chains, so that file would only be reached through a
//! link to the live worktree — and its unstaged edits would shape the verdict on
//! staged content. Naming it lets the selection write it from the index like the
//! config that extends it.

use std::path::{Component, Path, PathBuf};

use crate::git::{self, StagedEntry};

/// The ruff config file names a directory is searched for.
const CONFIG_NAMES: &[&str] = &["ruff.toml", ".ruff.toml", "pyproject.toml"];

/// The repo-relative files that the ruff configs among `selected` (and the files in
/// `extended`, already known to be extended) extend and that `extended` does not
/// yet hold.
pub(super) fn extended_configs(
    root: &Path,
    selected: &[StagedEntry],
    extended: &[String],
) -> Result<Vec<String>, git::Error> {
    let configs: Vec<PathBuf> = selected
        .iter()
        .filter(|entry| !entry.is_symlink)
        .filter(|entry| {
            entry
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| CONFIG_NAMES.contains(&name))
                || extended.iter().any(|path| Path::new(path) == entry.path)
        })
        .map(|entry| entry.path.clone())
        .collect();
    if configs.is_empty() {
        return Ok(Vec::new());
    }
    let blobs = git::staged_blobs(root, &configs)?;
    let mut found: Vec<String> = configs
        .iter()
        .zip(blobs)
        .filter_map(|(path, blob)| {
            let text = std::str::from_utf8(blob.as_deref()?).ok()?;
            let target = extend_target(path, text)?;
            resolve(path.parent().unwrap_or(Path::new("")), &target)
        })
        .filter(|path| !extended.contains(path))
        .collect();
    found.sort();
    found.dedup();
    Ok(found)
}

/// The `extend` value of the ruff config in `text`, a `pyproject.toml`'s under
/// `[tool.ruff]`.
fn extend_target(path: &Path, text: &str) -> Option<String> {
    let table: toml::Table = text.parse().ok()?;
    let ruff = if path.file_name().is_some_and(|name| name == "pyproject.toml") {
        table.get("tool")?.as_table()?.get("ruff")?.as_table()?
    } else {
        &table
    };
    ruff.get("extend")?.as_str().map(str::to_owned)
}

/// `target`, written relative to `dir`, as a repo-relative path; `None` when it is
/// absolute or climbs out of the repository.
fn resolve(dir: &Path, target: &str) -> Option<String> {
    let mut parts: Vec<&std::ffi::OsStr> = dir
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    for component in Path::new(target).components() {
        match component {
            Component::Normal(part) => parts.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.iter().collect::<PathBuf>().to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extend_is_read_from_ruff_toml_and_from_pyproject() {
        assert_eq!(
            extend_target(Path::new("a/ruff.toml"), "extend = \"../base.toml\"\n").as_deref(),
            Some("../base.toml")
        );
        assert_eq!(
            extend_target(
                Path::new("a/pyproject.toml"),
                "[tool.ruff]\nextend = \"../base.toml\"\n"
            )
            .as_deref(),
            Some("../base.toml")
        );
        assert_eq!(extend_target(Path::new("a/pyproject.toml"), "extend = \"x\"\n"), None);
    }

    #[test]
    fn a_target_is_resolved_against_the_config_directory() {
        assert_eq!(
            resolve(Path::new("pkg/sub"), "../../shared/base.toml").as_deref(),
            Some("shared/base.toml")
        );
        assert_eq!(
            resolve(Path::new("pkg"), "./base.toml").as_deref(),
            Some("pkg/base.toml")
        );
    }

    #[test]
    fn a_target_that_leaves_the_repository_or_is_absolute_is_ignored() {
        assert_eq!(resolve(Path::new("pkg"), "../../outside.toml"), None);
        assert_eq!(resolve(Path::new("pkg"), "/etc/ruff.toml"), None);
    }
}
