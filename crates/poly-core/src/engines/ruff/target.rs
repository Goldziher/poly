//! The Python version a file is linted for when `[lint.python.ruff]` sets no
//! `target_version`: the answer the project's own ruff configuration gives.
//!
//! Version-gated rules (`UP017` `datetime.UTC`, `UP042` `StrEnum`, …) and
//! isort's standard-library list all follow the target, so leaving it at ruff's
//! built-in default — 3.11 since the 0.17 line — tells a 3.10 project to make
//! rewrites that break on 3.10 (Goldziher/poly#42).
//!
//! The lookup follows ruff's own discovery, walking up from the file's
//! directory:
//!
//! 1. The nearest ruff config — `.ruff.toml`, `ruff.toml`, or a `pyproject.toml`
//!    with a `[tool.ruff]` table, in that order within a directory — decides
//!    via its `target-version`, following its `extend` chain when it sets none.
//! 2. Otherwise `project.requires-python` in the nearest `pyproject.toml`
//!    (starting from that config's directory, or from the file's when there is
//!    no ruff config), taken as its lowest admitted minor version.
//!
//! Answers are remembered per directory until the next plan, like the package
//! roots in [`super::context`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

use ruff_python_ast::PythonVersion;

/// How many `extend` hops are followed before giving up, so a cycle ends.
const MAX_EXTEND_DEPTH: usize = 8;

/// Targets already found, keyed by the directory they were found for.
static TARGETS: LazyLock<Mutex<HashMap<PathBuf, Option<PythonVersion>>>> = LazyLock::new(Mutex::default);

/// Drop every remembered target, so a config edited between runs of a
/// long-lived process is read again.
pub(super) fn forget_targets() {
    TARGETS.lock().unwrap_or_else(PoisonError::into_inner).clear();
}

/// The target version the project configuration governing `path` declares,
/// or `None` when it declares none (ruff's default then applies).
pub(super) fn discovered_target(path: &Path) -> Option<PythonVersion> {
    let parent = path.parent().unwrap_or(Path::new(""));
    let dir = if parent.is_absolute() {
        parent.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(parent)
    };
    if let Some(hit) = TARGETS.lock().unwrap_or_else(PoisonError::into_inner).get(&dir) {
        return *hit;
    }
    let found = discover(&dir);
    TARGETS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(dir, found);
    found
}

fn discover(dir: &Path) -> Option<PythonVersion> {
    let (config_dir, config) = nearest_ruff_config(dir).map_or((dir, None), |(d, t)| (d, Some(t)));
    if let Some((table, source)) = config
        && let Some(version) = configured_target(table, &source, 0)
    {
        return Some(version);
    }
    config_dir
        .ancestors()
        .find_map(|ancestor| load(&ancestor.join("pyproject.toml")))
        .and_then(|pyproject| requires_python(&pyproject))
}

/// The nearest directory at or above `dir` holding a ruff config, with that
/// config's ruff table and the file it came from.
fn nearest_ruff_config(dir: &Path) -> Option<(&Path, (toml::Table, PathBuf))> {
    dir.ancestors().find_map(|ancestor| {
        for name in [".ruff.toml", "ruff.toml"] {
            let path = ancestor.join(name);
            if let Some(table) = load(&path) {
                return Some((ancestor, (table, path)));
            }
        }
        let path = ancestor.join("pyproject.toml");
        let ruff = load(&path)?
            .remove("tool")
            .and_then(|tool| match tool {
                toml::Value::Table(mut tool) => tool.remove("ruff"),
                _ => None,
            })
            .and_then(|ruff| match ruff {
                toml::Value::Table(ruff) => Some(ruff),
                _ => None,
            })?;
        Some((ancestor, (ruff, path)))
    })
}

/// `target-version` from a ruff table, or from the config it `extend`s.
fn configured_target(table: toml::Table, source: &Path, depth: usize) -> Option<PythonVersion> {
    if let Some(version) = table
        .get("target-version")
        .or_else(|| table.get("target_version"))
        .and_then(toml::Value::as_str)
    {
        return super::parse_python_version(version);
    }
    if depth >= MAX_EXTEND_DEPTH {
        return None;
    }
    let extend = table.get("extend").and_then(toml::Value::as_str)?;
    let base = source.parent().unwrap_or(Path::new("")).join(extend);
    let mut loaded = load(&base)?;
    let is_pyproject = base.file_name().is_some_and(|name| name == "pyproject.toml");
    let ruff = if is_pyproject {
        match loaded.remove("tool")? {
            toml::Value::Table(mut tool) => match tool.remove("ruff")? {
                toml::Value::Table(ruff) => ruff,
                _ => return None,
            },
            _ => return None,
        }
    } else {
        loaded
    };
    configured_target(ruff, &base, depth + 1)
}

/// The lowest minor version `project.requires-python` admits, the way ruff
/// infers a target from it.
fn requires_python(pyproject: &toml::Table) -> Option<PythonVersion> {
    let requires = pyproject.get("project")?.as_table()?.get("requires-python")?.as_str()?;
    let mut lowest: Option<(u8, u8)> = None;
    for clause in requires.split(',').map(str::trim) {
        let Some((operator, version)) = ["~=", ">=", "==", ">"]
            .iter()
            .find_map(|op| clause.strip_prefix(op).map(|rest| (*op, rest.trim())))
        else {
            continue;
        };
        let mut parts = version.trim_end_matches(".*").split('.');
        let (Some(Ok(major)), Some(Ok(minor))) =
            (parts.next().map(str::parse::<u8>), parts.next().map(str::parse::<u8>))
        else {
            continue;
        };
        let bound = if operator == ">" {
            (major, minor.saturating_add(1))
        } else {
            (major, minor)
        };
        // Every clause must hold, so the tightest lower bound is the largest.
        lowest = lowest.max(Some(bound));
    }
    lowest.map(PythonVersion::from)
}

fn load(path: &Path) -> Option<toml::Table> {
    std::fs::read_to_string(path).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::requires_python;
    use ruff_python_ast::PythonVersion;

    fn infer(spec: &str) -> Option<PythonVersion> {
        requires_python(&format!("[project]\nrequires-python = \"{spec}\"\n").parse().unwrap())
    }

    #[test]
    fn requires_python_takes_the_lowest_admitted_minor() {
        assert_eq!(infer(">=3.10"), Some(PythonVersion::PY310));
        assert_eq!(infer(">=3.9,<4"), Some(PythonVersion::PY39));
        assert_eq!(infer("~=3.11.2"), Some(PythonVersion::PY311));
        assert_eq!(infer(">3.10"), Some(PythonVersion::PY311));
        assert_eq!(infer("==3.12.*"), Some(PythonVersion::PY312));
        assert_eq!(infer("<4"), None);
    }
}
