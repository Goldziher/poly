//! Host-side resolution of the [`ProjectEnvironment`] a project check sees.
//!
//! Components never spawn processes or probe `PATH`, so everything a check needs to know about the
//! machine is resolved here, by the host, before the call. Resolution reads the filesystem only:
//! the interpreter is never executed, because a subprocess here would make every project run pay a
//! spawn and would make the answer depend on the interpreter's own start-up behaviour. The version
//! therefore comes from the venv's `pyvenv.cfg`, or failing that from the `lib/pythonX.Y` directory
//! name.

use std::path::{Path, PathBuf};

use crate::engine::{ProjectEnvironment, PythonEnvironment};

/// Resolve the environment for a project rooted at `root` from the process environment.
pub(crate) fn resolve_environment(root: &Path) -> ProjectEnvironment {
    let virtual_env = std::env::var_os("VIRTUAL_ENV")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    ProjectEnvironment {
        python: resolve_python(root, virtual_env.as_deref(), &|name| which::which(name).ok()),
    }
}

/// Find the project's Python: `VIRTUAL_ENV`, then `.venv/` and `venv/` under `root`, then
/// `python3` (and `python`) as `path_lookup` finds them.
///
/// `path_lookup` stands in for a `PATH` search so the order can be tested without touching the
/// machine's real interpreters.
pub(crate) fn resolve_python(
    root: &Path,
    virtual_env: Option<&Path>,
    path_lookup: &dyn Fn(&str) -> Option<PathBuf>,
) -> Option<PythonEnvironment> {
    let candidates = virtual_env
        .map(Path::to_path_buf)
        .into_iter()
        .chain([root.join(".venv"), root.join("venv")]);
    for venv in candidates {
        if let Some(environment) = from_venv(&venv) {
            return Some(environment);
        }
    }
    let interpreter = path_lookup("python3").or_else(|| path_lookup("python"))?;
    Some(from_interpreter(interpreter))
}

/// The environment of a virtual environment directory, or `None` when `venv` has no interpreter.
fn from_venv(venv: &Path) -> Option<PythonEnvironment> {
    let interpreter = venv_interpreter(venv)?;
    let site_packages = site_packages_under(venv);
    let version = pyvenv_version(venv).or_else(|| version_from_lib_dirs(venv));
    Some(PythonEnvironment {
        interpreter,
        site_packages,
        version,
    })
}

/// `venv/bin/python` (or `Scripts\python.exe`), when it exists.
fn venv_interpreter(venv: &Path) -> Option<PathBuf> {
    ["bin/python", "bin/python3", "Scripts/python.exe"]
        .iter()
        .map(|relative| venv.join(relative))
        .find(|candidate| candidate.is_file())
}

/// An interpreter found on `PATH`. If it lives in a venv (`<venv>/bin/python`, with a
/// `pyvenv.cfg` one level up) the venv's own site-packages apply; otherwise the install prefix's.
fn from_interpreter(interpreter: PathBuf) -> PythonEnvironment {
    let prefix = interpreter
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    // A symlinked system interpreter (`/usr/bin/python3` -> `/usr/local/bin/python3.12`) keeps its
    // lib directory beside the real file, so a venv is recognised by the unresolved location and
    // a system install by the resolved one.
    let prefix = if prefix.join("pyvenv.cfg").is_file() {
        prefix
    } else {
        interpreter
            .canonicalize()
            .ok()
            .and_then(|real| real.parent().and_then(Path::parent).map(Path::to_path_buf))
            .unwrap_or(prefix)
    };
    PythonEnvironment {
        site_packages: site_packages_under(&prefix),
        version: pyvenv_version(&prefix).or_else(|| version_from_lib_dirs(&prefix)),
        interpreter,
    }
}

/// Existing `site-packages` / `dist-packages` directories under an environment prefix, sorted.
fn site_packages_under(prefix: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let windows = prefix.join("Lib").join("site-packages");
    if windows.is_dir() {
        found.push(windows);
    }
    for lib in ["lib", "lib64"] {
        for python_dir in python_lib_dirs(&prefix.join(lib)) {
            for leaf in ["site-packages", "dist-packages"] {
                let candidate = python_dir.join(leaf);
                if candidate.is_dir() {
                    found.push(candidate);
                }
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// The `pythonX.Y` directories directly under `lib`, sorted by name.
fn python_lib_dirs(lib: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(lib) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("python") && entry.path().is_dir())
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs
}

/// `version` (or `version_info`) from `<venv>/pyvenv.cfg`.
fn pyvenv_version(venv: &Path) -> Option<String> {
    let text = std::fs::read_to_string(venv.join("pyvenv.cfg")).ok()?;
    parse_pyvenv_version(&text)
}

/// The version key of a `pyvenv.cfg` body. `version = 3.12.4` is written by `venv`,
/// `version_info = 3.12.4.final.0` by `virtualenv`; the latter is cut to three components.
fn parse_pyvenv_version(text: &str) -> Option<String> {
    let value = |wanted: &str| {
        text.lines().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == wanted).then(|| value.trim().to_string())
        })
    };
    if let Some(version) = value("version").filter(|v| !v.is_empty()) {
        return Some(version);
    }
    let info = value("version_info").filter(|v| !v.is_empty())?;
    Some(info.split('.').take(3).collect::<Vec<_>>().join("."))
}

/// `3.12` from a lone `lib/python3.12` directory, when the cfg file gives no version.
fn version_from_lib_dirs(prefix: &Path) -> Option<String> {
    let dirs = python_lib_dirs(&prefix.join("lib"));
    let [only] = dirs.as_slice() else {
        return None;
    };
    let name = only.file_name()?.to_string_lossy();
    let version = name.strip_prefix("python")?;
    (!version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.')).then(|| version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_venv(venv: &Path, cfg: Option<&str>) {
        std::fs::create_dir_all(venv.join("bin")).unwrap();
        std::fs::write(venv.join("bin/python"), "").unwrap();
        std::fs::create_dir_all(venv.join("lib/python3.12/site-packages")).unwrap();
        if let Some(cfg) = cfg {
            std::fs::write(venv.join("pyvenv.cfg"), cfg).unwrap();
        }
    }

    fn no_path(_: &str) -> Option<PathBuf> {
        None
    }

    #[test]
    fn project_venv_is_found_with_site_packages_and_cfg_version() {
        let dir = tempfile::tempdir().unwrap();
        make_venv(&dir.path().join(".venv"), Some("home = /usr/bin\nversion = 3.12.4\n"));
        let env = resolve_python(dir.path(), None, &no_path).expect("a venv");
        assert_eq!(env.interpreter, dir.path().join(".venv/bin/python"));
        assert_eq!(
            env.site_packages,
            vec![dir.path().join(".venv/lib/python3.12/site-packages")]
        );
        assert_eq!(env.version.as_deref(), Some("3.12.4"));
    }

    #[test]
    fn virtual_env_variable_beats_project_venv() {
        let dir = tempfile::tempdir().unwrap();
        make_venv(&dir.path().join(".venv"), None);
        let active = dir.path().join("elsewhere");
        make_venv(&active, None);
        let env = resolve_python(dir.path(), Some(&active), &no_path).expect("a venv");
        assert_eq!(env.interpreter, active.join("bin/python"));
    }

    #[test]
    fn version_falls_back_to_the_lib_directory_name() {
        let dir = tempfile::tempdir().unwrap();
        make_venv(&dir.path().join("venv"), None);
        let env = resolve_python(dir.path(), None, &no_path).expect("a venv");
        assert_eq!(env.version.as_deref(), Some("3.12"));
    }

    #[test]
    fn path_interpreter_is_the_last_resort() {
        let dir = tempfile::tempdir().unwrap();
        let python = dir.path().join("usr/bin/python3");
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, "").unwrap();
        std::fs::create_dir_all(dir.path().join("usr/lib/python3.11/dist-packages")).unwrap();
        let lookup = |name: &str| (name == "python3").then(|| python.clone());
        let env = resolve_python(&dir.path().join("project"), None, &lookup).expect("PATH python");
        assert_eq!(env.interpreter, python);
        assert_eq!(env.site_packages.len(), 1);
        assert!(env.site_packages[0].ends_with("python3.11/dist-packages"));
        assert_eq!(env.version.as_deref(), Some("3.11"));
    }

    #[test]
    fn nothing_found_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_python(dir.path(), None, &no_path).is_none());
    }

    #[test]
    fn virtualenv_style_version_info_is_cut_to_three_components() {
        assert_eq!(
            parse_pyvenv_version("version_info = 3.11.9.final.0\n").as_deref(),
            Some("3.11.9")
        );
    }
}
