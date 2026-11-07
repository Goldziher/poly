//! The pyrefly configuration a project check derives from its request.
//!
//! pyrefly builds its configuration from `pyrefly.toml` / `[tool.pyrefly]` and queries an
//! interpreter for the rest. A project check is a pure function of `(files, config, environment)`,
//! so neither happens: the inputs are the `[engines.python-types]` options and the interpreter the
//! host resolved, and `skip_interpreter_query` is forced so pyrefly never starts a process.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, anyhow, bail};
use poly_engine_api::engine::ProjectRequest;
use pyrefly_config::config::ConfigFile;
use pyrefly_config::error_kind::Severity;
use pyrefly_python::sys_info::PythonVersion;

/// `path` as an absolute path, resolved against the process working directory.
pub(crate) fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("cannot make {} absolute", path.display()))
}

/// Whether `text` is a `MAJOR.MINOR` or `MAJOR.MINOR.PATCH` version, as ty's backend accepts.
fn is_version(text: &str) -> bool {
    let mut parts = text.trim().split('.');
    let numeric = |part: Option<&str>| part.is_some_and(|p| p.parse::<u8>().is_ok());
    numeric(parts.next()) && numeric(parts.next())
}

fn string_list(request: &ProjectRequest<'_>, key: &str) -> anyhow::Result<Option<Vec<String>>> {
    let Some(value) = request.config.options.get(key) else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or_else(|| anyhow!("`{key}` must be an array of strings"))?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("`{key}` must be an array of strings"))
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map(Some)
}

fn under(root: &Path, entries: &[String]) -> Vec<PathBuf> {
    entries.iter().map(|entry| root.join(entry)).collect()
}

/// The Python version: `python_version` when configured, else the interpreter's, else pyrefly's default.
fn python_version(request: &ProjectRequest<'_>) -> anyhow::Result<Option<PythonVersion>> {
    let configured = match request.config.options.get("python_version") {
        None => None,
        Some(value) => {
            let text = value
                .as_str()
                .ok_or_else(|| anyhow!("`python_version` must be a string such as \"3.12\""))?;
            if !is_version(text) {
                bail!("`python_version` is not a MAJOR.MINOR version: {text:?}");
            }
            Some(text.to_owned())
        }
    };
    let resolved = request
        .environment
        .python
        .as_ref()
        .and_then(|python| python.version.clone())
        .filter(|text| is_version(text));
    configured
        .or(resolved)
        .map(|text| PythonVersion::from_str(&text))
        .transpose()
}

/// The configuration for `request`, ready to hand to a `ConfigFinder`.
///
/// Import roots, in pyrefly's priority order: `extra_paths`, then the first-party roots (`src`,
/// default the project root plus `src/` when it exists), then the host-resolved `site-packages`.
/// pyrefly's own search-path heuristics are off so the roots are exactly these.
pub(crate) fn config(request: &ProjectRequest<'_>, root: &Path) -> anyhow::Result<ConfigFile> {
    let mut config = ConfigFile::default();
    config.interpreters.skip_interpreter_query = true;
    config.disable_search_path_heuristics = true;
    config.python_environment.python_version = python_version(request)?;
    config.python_environment.site_package_path = Some(
        request
            .environment
            .python
            .as_ref()
            .map(|python| python.site_packages.iter().map(|path| absolute(path)).collect())
            .transpose()?
            .unwrap_or_default(),
    );

    let src_roots = match string_list(request, "src")? {
        Some(entries) => under(root, &entries),
        None => {
            let mut roots = vec![root.to_path_buf()];
            let conventional = root.join("src");
            if conventional.is_dir() {
                roots.push(conventional);
            }
            roots
        }
    };
    let mut search_path = under(root, &string_list(request, "extra_paths")?.unwrap_or_default());
    search_path.extend(src_roots);
    config.search_path_from_file = search_path;

    if let Some(error) = config
        .configure_at(Some(root))
        .into_iter()
        .find(|error| error.severity() == Severity::Error)
    {
        bail!("invalid pyrefly configuration: {}", error.get_message());
    }
    Ok(config)
}
