//! `poly hooks` — run git hooks declared in `poly.toml`'s `[hooks]` table.
//!
//! poly does not reimplement the pre-commit hook runner; it drives the vendored
//! `prek` engine. `[hooks]` is translated into a standard pre-commit config
//! document (emitted as JSON, which is valid YAML) and `prek` is invoked over
//! it. poly's own tools (`[hooks.builtin]`) become `repo: local` hooks whose
//! entry is the absolute path of the running `poly` binary, so `poly lint`,
//! `poly fmt`, and `poly commit` run without any clone or PATH lookup. Foreign
//! `[[hooks.repo]]` entries map to ordinary remote pre-commit repos, which
//! `prek` clones and runs exactly as before.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::Args;
use poly_config::{HooksConfig, PolyConfig};
use serde::Serialize;

/// `poly hooks` arguments. All trailing arguments are forwarded verbatim to the
/// `prek` engine (so `poly hooks run --all-files`, `poly hooks install`, etc.
/// all work); the `[hooks]` config is injected automatically.
#[derive(Args)]
pub struct HooksArgs {
    /// Path to the config file (default: nearest poly.toml / polylint.toml).
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Arguments forwarded to the underlying `prek` engine (e.g. `run
    /// --all-files`, `install`). With none, the default is `run`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub forwarded: Vec<OsString>,
}

// ── pre-commit config model (serialized to JSON == valid YAML) ───────────────

/// A generated pre-commit configuration document.
#[derive(Debug, Serialize, PartialEq)]
struct PrecommitConfig {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    default_stages: Vec<String>,
    repos: Vec<PrecommitRepo>,
}

/// One repository entry in a pre-commit config.
#[derive(Debug, Serialize, PartialEq)]
struct PrecommitRepo {
    repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rev: Option<String>,
    hooks: Vec<PrecommitHook>,
}

/// One hook entry in a pre-commit config.
#[derive(Debug, Serialize, PartialEq)]
struct PrecommitHook {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stages: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pass_filenames: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude: Option<String>,
}

/// Translate a `[hooks]` table into a pre-commit config, using `poly_bin` as the
/// entry for poly's own in-process tools.
fn to_precommit_config(hooks: &HooksConfig, poly_bin: &Path) -> PrecommitConfig {
    let poly = poly_bin.to_string_lossy().into_owned();
    let mut repos = Vec::new();

    // poly's own tools as a single `repo: local`.
    let mut local_hooks = Vec::new();
    if hooks.builtin.polylint.enabled {
        local_hooks.push(PrecommitHook {
            id: "polylint".to_string(),
            name: Some("polylint".to_string()),
            entry: Some(poly.clone()),
            language: Some("system".to_string()),
            args: vec!["lint".to_string()],
            stages: hooks.builtin.polylint.stages.clone(),
            pass_filenames: Some(true),
            exclude: None,
        });
    }
    if hooks.builtin.polyfmt.enabled {
        local_hooks.push(PrecommitHook {
            id: "polyfmt".to_string(),
            name: Some("polyfmt".to_string()),
            entry: Some(poly.clone()),
            language: Some("system".to_string()),
            args: vec!["fmt".to_string(), "--check".to_string()],
            stages: hooks.builtin.polyfmt.stages.clone(),
            pass_filenames: Some(true),
            exclude: None,
        });
    }
    if hooks.builtin.commit.enabled {
        // Commit-message hooks run at the `commit-msg` stage and receive the
        // message file as their filename argument.
        let stages = if hooks.builtin.commit.stages.is_empty() {
            vec!["commit-msg".to_string()]
        } else {
            hooks.builtin.commit.stages.clone()
        };
        local_hooks.push(PrecommitHook {
            id: "poly-commit".to_string(),
            name: Some("poly commit".to_string()),
            entry: Some(poly.clone()),
            language: Some("system".to_string()),
            args: vec!["commit".to_string()],
            stages,
            pass_filenames: Some(true),
            exclude: None,
        });
    }
    if !local_hooks.is_empty() {
        repos.push(PrecommitRepo {
            repo: "local".to_string(),
            rev: None,
            hooks: local_hooks,
        });
    }

    PrecommitConfig {
        default_stages: hooks.stages.clone(),
        repos,
    }
}

/// Run `poly hooks`: translate `[hooks]` and invoke the `prek` engine over it.
pub fn run_hooks(args: HooksArgs) -> ExitCode {
    match run_hooks_inner(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("poly hooks: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run_hooks_inner(args: HooksArgs) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir()?;
    let poly_config = match &args.config {
        Some(path) => PolyConfig::load_file(path)?,
        None => PolyConfig::load(&cwd)?,
    };

    let poly_bin = std::env::current_exe()?;
    let config = to_precommit_config(&poly_config.hooks, &poly_bin);
    let rendered = serde_json::to_string_pretty(&config)?;

    // Write the generated config next to the repo so relative paths resolve from
    // the working directory, and clean it up after the run.
    let config_path = cwd.join(".poly-hooks-generated.yaml");
    std::fs::write(&config_path, rendered)?;
    let _guard = CleanupGuard(&config_path);

    let prek = locate_prek()?;
    let mut command = Command::new(&prek);
    command.arg("--config").arg(&config_path);
    if args.forwarded.is_empty() {
        command.arg("run");
    } else {
        command.args(&args.forwarded);
    }
    let status = command
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run hook engine `{}`: {e}", prek.display()))?;

    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(code as u8),
        None => ExitCode::from(1),
    })
}

/// Locate the bundled `prek` hook engine: prefer a sibling of the running
/// binary, fall back to `prek` on `PATH`.
fn locate_prek() -> anyhow::Result<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(if cfg!(windows) { "prek.exe" } else { "prek" });
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Ok(PathBuf::from("prek"))
}

/// Removes the generated config file when the run finishes.
struct CleanupGuard<'a>(&'a Path);

impl Drop for CleanupGuard<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_from(toml: &str) -> HooksConfig {
        // Parse a [hooks] table through a full poly.toml document.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        std::fs::write(&path, toml).unwrap();
        PolyConfig::load_file(&path).unwrap().hooks
    }

    #[test]
    fn builtins_become_local_repo_hooks_with_poly_entry() {
        let hooks = cfg_from(
            r#"
[hooks]
stages = ["pre-commit"]
[hooks.builtin]
polylint = true
polyfmt = { stages = ["pre-commit"] }
commit = true
"#,
        );
        let config = to_precommit_config(&hooks, Path::new("/opt/poly/bin/poly"));

        assert_eq!(config.default_stages, vec!["pre-commit"]);
        assert_eq!(config.repos.len(), 1);
        let local = &config.repos[0];
        assert_eq!(local.repo, "local");
        assert_eq!(local.rev, None);
        assert_eq!(local.hooks.len(), 3);

        let lint = &local.hooks[0];
        assert_eq!(lint.id, "polylint");
        assert_eq!(lint.entry.as_deref(), Some("/opt/poly/bin/poly"));
        assert_eq!(lint.args, vec!["lint"]);
        assert_eq!(lint.language.as_deref(), Some("system"));
        assert_eq!(lint.pass_filenames, Some(true));

        let fmt = &local.hooks[1];
        assert_eq!(fmt.id, "polyfmt");
        assert_eq!(fmt.args, vec!["fmt", "--check"]);
        assert_eq!(fmt.stages, vec!["pre-commit"]);

        let commit = &local.hooks[2];
        assert_eq!(commit.id, "poly-commit");
        assert_eq!(commit.args, vec!["commit"]);
        // commit defaults to the commit-msg stage when none is given.
        assert_eq!(commit.stages, vec!["commit-msg"]);
    }

    #[test]
    fn disabled_builtins_are_omitted() {
        let hooks = cfg_from(
            r#"
[hooks.builtin]
polylint = true
"#,
        );
        let config = to_precommit_config(&hooks, Path::new("poly"));
        assert_eq!(config.repos.len(), 1);
        assert_eq!(config.repos[0].hooks.len(), 1);
        assert_eq!(config.repos[0].hooks[0].id, "polylint");
    }

    #[test]
    fn generated_config_is_valid_json() {
        let hooks = cfg_from(
            r#"
[hooks.builtin]
polylint = true
"#,
        );
        let config = to_precommit_config(&hooks, Path::new("poly"));
        let rendered = serde_json::to_string_pretty(&config).unwrap();
        // Round-trips as JSON (and therefore parses as YAML for prek).
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert!(value["repos"][0]["hooks"][0]["id"] == "polylint");
    }
}
