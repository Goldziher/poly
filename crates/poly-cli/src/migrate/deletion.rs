//! Deletion policy: which source files may be removed after their settings are
//! absorbed, which are always kept (poly delegates to the native tool, or they
//! are publisher/build files), and which are only reported.
//!
//! The policy is data — a static KEEP list plus a per-source verdict that folds
//! in the importer's [`Absorb`] completeness. A source is deleted only when
//! every meaningful setting was absorbed; anything Partial is kept intact.

use std::path::{Path, PathBuf};

use super::importers::{Absorb, ImportResult};

/// What to do with a source config file during `--write`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Delete the whole file — every setting was absorbed.
    DeleteFile(PathBuf),
    /// Strip the given sections from `pyproject.toml` (never delete the file).
    /// Each section is a dotted path under the document root, e.g.
    /// `["tool", "ruff"]`.
    StripPyproject { path: PathBuf, sections: Vec<Vec<String>> },
    /// Keep the file untouched, with a reason (delegated tool, or Partial absorb).
    Keep { path: PathBuf, reason: String },
    /// Keep and merely report the file (never auto-modified).
    ReportOnly { path: PathBuf, note: String },
}

impl Action {
    /// The source path this action concerns.
    pub fn path(&self) -> &Path {
        match self {
            Action::DeleteFile(p)
            | Action::StripPyproject { path: p, .. }
            | Action::Keep { path: p, .. }
            | Action::ReportOnly { path: p, .. } => p,
        }
    }
}

/// Files poly never deletes, paired with the reason it delegates or defers.
/// Relative paths are resolved against the migration directory.
const KEEP_FILES: &[(&str, &str)] = &[
    ("rustfmt.toml", "rustfmt owns its own config (native toolchain backend)"),
    (
        ".rustfmt.toml",
        "rustfmt owns its own config (native toolchain backend)",
    ),
    ("clippy.toml", "clippy config; poly delegates to the cargo hook"),
    (".clippy.toml", "clippy config; poly delegates to the cargo hook"),
    (".golangci.yml", "golangci-lint owns its config (catalog tool)"),
    (".golangci.yaml", "golangci-lint owns its config (catalog tool)"),
    (
        ".pre-commit-hooks.yaml",
        "hook publisher manifest, not a consumer config",
    ),
    (".pylintrc", "pylint is not a poly backend"),
    (".cargo/config.toml", "cargo build configuration"),
    (".eslintrc", "eslint owns its config"),
    (".eslintrc.json", "eslint owns its config"),
    (".eslintrc.js", "eslint owns its config"),
    (".eslintrc.cjs", "eslint owns its config"),
    (".eslintrc.yml", "eslint owns its config"),
    (".eslintrc.yaml", "eslint owns its config"),
    ("eslint.config.js", "eslint owns its config"),
    ("eslint.config.mjs", "eslint owns its config"),
    (".prettierrc", "prettier owns its config"),
    (".prettierrc.json", "prettier owns its config"),
    (".prettierrc.yml", "prettier owns its config"),
    (".prettierrc.yaml", "prettier owns its config"),
    ("prettier.config.js", "prettier owns its config"),
    ("biome.json", "biome owns its config"),
    ("biome.jsonc", "biome owns its config"),
    ("Cargo.lock", "lock file — never linted or formatted"),
    ("package-lock.json", "lock file — never linted or formatted"),
    ("pnpm-lock.yaml", "lock file — never linted or formatted"),
    ("yarn.lock", "lock file — never linted or formatted"),
    ("uv.lock", "lock file — never linted or formatted"),
    ("poetry.lock", "lock file — never linted or formatted"),
    ("go.sum", "lock file — never linted or formatted"),
];

/// Whether `file_name` is on the never-delete KEEP list. Returns the reason.
pub fn keep_reason(file_name: &str) -> Option<&'static str> {
    KEEP_FILES
        .iter()
        .find(|(name, _)| *name == file_name)
        .map(|(_, reason)| *reason)
}

/// Decide the deletion action for a single absorbed source, folding in its
/// completeness verdict.
pub fn action_for_source(source: &Path, tool: &str, absorb: &Absorb) -> Action {
    let path = source.to_path_buf();
    let is_pyproject = source.file_name().is_some_and(|n| n == "pyproject.toml");

    match absorb {
        Absorb::Partial(keys) => Action::Keep {
            path,
            reason: format!("partial absorb — poly cannot represent: {}", keys.join(", ")),
        },
        Absorb::None => Action::Keep {
            path,
            reason: "nothing absorbed".to_string(),
        },
        Absorb::Full => {
            if is_pyproject {
                Action::StripPyproject {
                    path,
                    sections: pyproject_sections(tool),
                }
            } else {
                Action::DeleteFile(path)
            }
        }
    }
}

/// The `pyproject.toml` sections a tool's absorbed config occupies.
fn pyproject_sections(tool: &str) -> Vec<Vec<String>> {
    match tool {
        "ruff" => vec![vec!["tool".to_string(), "ruff".to_string()]],
        "typos" => vec![
            vec!["tool".to_string(), "typos".to_string()],
            vec!["tool".to_string(), "codespell".to_string()],
        ],
        _ => Vec::new(),
    }
}

/// Build the deletion actions for every absorbed source in `results`.
pub fn plan_actions(results: &[ImportResult]) -> Vec<Action> {
    let mut actions = Vec::new();
    for result in results {
        for source in &result.sources {
            actions.push(action_for_source(source, result.tool, &result.absorb));
        }
    }
    actions
}

/// Scan `dir` for always-keep files and the report-only `.pre-commit-config.yaml`.
pub fn scan_kept(dir: &Path) -> Vec<Action> {
    let mut actions = Vec::new();
    for (name, reason) in KEEP_FILES {
        let path = dir.join(name);
        if path.is_file() {
            actions.push(Action::Keep {
                path,
                reason: (*reason).to_string(),
            });
        }
    }
    let precommit = dir.join(".pre-commit-config.yaml");
    if precommit.is_file() {
        actions.push(Action::ReportOnly {
            path: precommit,
            note: "review hooks manually — poly does not auto-migrate .pre-commit-config.yaml".to_string(),
        });
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn ruff_toml_full_absorb_is_deleted() {
        let action = action_for_source(&PathBuf::from("/repo/ruff.toml"), "ruff", &Absorb::Full);
        assert_eq!(action, Action::DeleteFile(PathBuf::from("/repo/ruff.toml")));
    }

    #[test]
    fn pyproject_full_absorb_strips_sections() {
        let action = action_for_source(&PathBuf::from("/repo/pyproject.toml"), "ruff", &Absorb::Full);
        assert_eq!(
            action,
            Action::StripPyproject {
                path: PathBuf::from("/repo/pyproject.toml"),
                sections: vec![vec!["tool".to_string(), "ruff".to_string()]],
            }
        );
    }

    #[test]
    fn partial_absorb_is_kept() {
        let action = action_for_source(
            &PathBuf::from("/repo/.markdownlint.json"),
            "markdownlint",
            &Absorb::Partial(vec!["default = false".to_string()]),
        );
        assert!(matches!(action, Action::Keep { .. }), "partial absorb must be kept");
    }

    #[test]
    fn rustfmt_is_on_keep_list() {
        assert!(keep_reason("rustfmt.toml").is_some());
        assert!(keep_reason("clippy.toml").is_some());
        assert!(keep_reason("ruff.toml").is_none());
    }
}
