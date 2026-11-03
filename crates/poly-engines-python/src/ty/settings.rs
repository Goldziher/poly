//! The ty program settings a project check derives from its request.
//!
//! `ty_project` builds these from `ty.toml` / `[tool.ty]`. A project check is a pure function of
//! `(files, config, environment)`, so the inputs here are the `[engines.python-types]` options and
//! the interpreter the host resolved; ty's own config files are not read.

use std::path::Path;

use anyhow::{Context, anyhow};
use poly_engine_api::engine::ProjectRequest;
use ruff_db::system::{System, SystemPath, SystemPathBuf};
use ruff_db::vendored::VendoredFileSystem;
use ruff_python_ast::PythonVersion;
use ty_module_resolver::{FallibleStrategy, SearchPathSettings};
use ty_python_core::platform::PythonPlatform;
use ty_python_core::program::ProgramSettings;
use ty_site_packages::{PythonVersionSource, PythonVersionWithSource};

/// `path` as an absolute UTF-8 system path, resolved against the process working directory.
pub(super) fn system_path(path: &Path) -> anyhow::Result<SystemPathBuf> {
    let absolute = std::path::absolute(path).with_context(|| format!("cannot make {} absolute", path.display()))?;
    SystemPathBuf::from_path_buf(absolute).map_err(|path| anyhow!("path is not valid UTF-8: {}", path.display()))
}

/// `MAJOR.MINOR` from a version string such as `3.12` or `3.12.4`.
pub(super) fn parse_version(text: &str) -> Option<PythonVersion> {
    let mut parts = text.trim().split('.');
    let major = parts.next()?.parse::<u8>().ok()?;
    let minor = parts.next()?.parse::<u8>().ok()?;
    Some(PythonVersion::from((major, minor)))
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

fn under(root: &SystemPath, entries: &[String]) -> Vec<SystemPathBuf> {
    entries.iter().map(|entry| SystemPath::absolute(entry, root)).collect()
}

/// The Python version: `python_version` when configured, else the interpreter's, else ty's default.
fn python_version(request: &ProjectRequest<'_>) -> anyhow::Result<PythonVersionWithSource> {
    let configured = match request.config.options.get("python_version") {
        None => None,
        Some(value) => {
            let text = value
                .as_str()
                .ok_or_else(|| anyhow!("`python_version` must be a string such as \"3.12\""))?;
            Some(
                parse_version(text)
                    .ok_or_else(|| anyhow!("`python_version` is not a MAJOR.MINOR version: {text:?}"))?,
            )
        }
    };
    let resolved = request
        .environment
        .python
        .as_ref()
        .and_then(|python| python.version.as_deref())
        .and_then(parse_version);
    Ok(match configured.or(resolved) {
        Some(version) => PythonVersionWithSource {
            version,
            source: PythonVersionSource::Cli,
        },
        None => PythonVersionWithSource::default(),
    })
}

/// The program settings for `request`.
///
/// Search paths, in ty's priority order: `extra_paths`, the first-party roots (`src`, default the
/// project root plus `src/` when it exists), then the host-resolved `site-packages` directories.
pub(super) fn program_settings(
    request: &ProjectRequest<'_>,
    root: &SystemPath,
    system: &dyn System,
    vendored: &VendoredFileSystem,
) -> anyhow::Result<ProgramSettings> {
    let src_roots = match string_list(request, "src")? {
        Some(entries) => under(root, &entries),
        None => {
            let mut roots = vec![root.to_path_buf()];
            let conventional = root.join("src");
            if system.is_directory(&conventional) {
                roots.push(conventional);
            }
            roots
        }
    };
    let extra_paths = under(root, &string_list(request, "extra_paths")?.unwrap_or_default());
    let site_packages_paths = match &request.environment.python {
        Some(python) => python
            .site_packages
            .iter()
            .map(|path| system_path(path))
            .collect::<anyhow::Result<Vec<_>>>()?,
        None => Vec::new(),
    };
    let search_paths = SearchPathSettings {
        extra_paths,
        src_roots,
        site_packages_paths,
        ..SearchPathSettings::empty()
    }
    .to_search_paths(system, vendored, &FallibleStrategy)
    .map_err(|error| anyhow!("invalid module search paths: {error}"))?;
    Ok(ProgramSettings {
        python_version: python_version(request)?,
        python_platform: PythonPlatform::default(),
        search_paths,
        python_environment: Ok(None),
    })
}
