//! Finding the program that runs a recipe tool.
//!
//! The order, first match wins:
//!
//! 1. `[tools.<name>] bin` — an explicit executable (for a JVM tool, the `java`).
//! 2. The pinned release in poly's tool cache, when `version` names one — a pin
//!    in the config beats whatever happens to be on `PATH`.
//! 3. The pinned release downloaded now, when `install = "auto"`.
//! 4. The tool's binary on `PATH`, only when no `version` is pinned: a pin that
//!    cannot be honoured makes the tool unavailable instead of running another
//!    version.
//!
//! A tool that is nowhere is [`Resolved::Missing`] with a message that says what
//! to run, never a silent no-op: a linter that did not run must not look like a
//! linter that found nothing.

use std::path::{Path, PathBuf};

use poly_catalog::{Asset, Recipe, Release, ReleaseKind, Tool};
use poly_config::{InstallPolicy, ToolConfig};

use super::install;

/// How to start the tool: a program and the arguments that precede its own.
#[derive(Debug, Clone)]
pub(in crate::engines::catalog_tool) struct Launcher {
    pub(in crate::engines::catalog_tool) program: PathBuf,
    /// `-jar <jar>` for a JVM tool, empty otherwise.
    pub(in crate::engines::catalog_tool) prefix: Vec<String>,
    /// What the cache key folds in so a different tool build never reuses
    /// another's results.
    pub(in crate::engines::catalog_tool) identity: String,
}

pub(in crate::engines::catalog_tool) enum Resolved {
    Ready(Launcher),
    Missing(String),
}

pub(in crate::engines::catalog_tool) fn resolve(tool: &Tool, recipe: &Recipe, config: &ToolConfig) -> Resolved {
    let pinned = match pinned_release(tool, recipe, config) {
        Ok(pinned) => pinned,
        Err(message) => return Resolved::Missing(message),
    };
    if recipe.java {
        return resolve_jvm(tool, recipe, config, pinned);
    }
    if let Some(bin) = &config.bin {
        return match locate(bin) {
            Some(path) => ready(path, Vec::new(), "bin"),
            None => Resolved::Missing(format!("`bin = \"{bin}\"` for tool `{}` does not exist", tool.name)),
        };
    }
    if let Some(pin) = &pinned {
        return match provision(tool, config, pin) {
            Ok(path) => ready(path, Vec::new(), &pin.version),
            Err(reason) => Resolved::Missing(unmet_pin(tool, pin, &reason)),
        };
    }
    match which::which(&tool.binary) {
        Ok(path) => ready(path, Vec::new(), "path"),
        Err(_) => Resolved::Missing(missing_message(tool, recipe, pinned.as_ref())),
    }
}

fn resolve_jvm(tool: &Tool, recipe: &Recipe, config: &ToolConfig, pinned: Option<Pinned<'_>>) -> Resolved {
    if let Some(pin) = &pinned {
        let jar = match provision(tool, config, pin) {
            Ok(jar) => jar,
            Err(reason) => return Resolved::Missing(unmet_pin(tool, pin, &reason)),
        };
        let Some(java) = java_program(config) else {
            return Resolved::Missing(format!(
                "tool `{}` is a JVM tool but no `java` was found; set `bin` to one or put it on PATH",
                tool.name
            ));
        };
        let prefix = vec!["-jar".to_owned(), jar.to_string_lossy().into_owned()];
        return ready(java, prefix, &pin.version);
    }
    match which::which(&tool.binary) {
        Ok(path) => ready(path, Vec::new(), "path"),
        Err(_) => Resolved::Missing(missing_message(tool, recipe, pinned.as_ref())),
    }
}

/// The `java` to launch a jar with: `bin`, else `JAVA_HOME` from the tool's
/// `env` or the process's, else `PATH`.
fn java_program(config: &ToolConfig) -> Option<PathBuf> {
    if let Some(bin) = &config.bin {
        return locate(bin);
    }
    let home = config
        .env
        .get("JAVA_HOME")
        .cloned()
        .or_else(|| std::env::var("JAVA_HOME").ok());
    if let Some(home) = home {
        let candidate = Path::new(&home).join("bin/java");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    which::which("java").ok()
}

fn locate(bin: &str) -> Option<PathBuf> {
    let path = Path::new(bin);
    if path.components().count() > 1 {
        path.is_file().then(|| path.to_path_buf())
    } else {
        which::which(bin).ok()
    }
}

fn ready(program: PathBuf, prefix: Vec<String>, origin: &str) -> Resolved {
    let identity = identity(&program, &prefix, origin);
    Resolved::Ready(Launcher {
        program,
        prefix,
        identity,
    })
}

/// A string that changes when the executable (or the jar it launches) does:
/// its resolved path, size and modification time. `--version` would be more
/// readable but not every tool answers it, and spawning a JVM to ask costs
/// 400 ms on every run.
fn identity(program: &Path, prefix: &[String], origin: &str) -> String {
    let target = match prefix {
        [_, jar] => Path::new(jar),
        _ => program,
    };
    let stamp = std::fs::metadata(target).ok().map(|meta| {
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        format!("{}:{modified}", meta.len())
    });
    format!(
        "{origin}:{}:{}",
        std::fs::canonicalize(target)
            .unwrap_or_else(|_| target.to_path_buf())
            .display(),
        stamp.unwrap_or_default()
    )
}

/// A release the config pins, with the asset that serves this host.
struct Pinned<'a> {
    version: String,
    asset: Asset,
    kind: ReleaseKind,
    /// The catalog's table, kept only to name the known versions in messages.
    known: Option<&'a Release>,
}

fn pinned_release<'a>(tool: &'a Tool, recipe: &'a Recipe, config: &ToolConfig) -> Result<Option<Pinned<'a>>, String> {
    let Some(version) = &config.version else {
        return Ok(None);
    };
    if let (Some(url), Some(sha256)) = (&config.url, &config.sha256) {
        let kind = if recipe.java {
            ReleaseKind::Jar
        } else {
            ReleaseKind::Binary
        };
        return Ok(Some(Pinned {
            version: version.clone(),
            asset: Asset {
                url: url.clone(),
                sha256: sha256.to_ascii_lowercase(),
                member: None,
            },
            kind,
            known: None,
        }));
    }
    let release = recipe
        .install
        .as_ref()
        .and_then(|install| install.releases.get(version));
    let Some(release) = release else {
        let known: Vec<&str> = recipe
            .install
            .iter()
            .flat_map(|install| install.releases.keys().map(String::as_str))
            .collect();
        return Err(format!(
            "tool `{}` has no pinned release {version}; known: [{}]. Give `url` and `sha256` to pin another.",
            tool.name,
            known.join(", ")
        ));
    };
    let Some(asset) = install::asset_for(release) else {
        return Err(format!(
            "tool `{}` {version} has no download for {}; install it yourself and set `bin`",
            tool.name,
            install::platform()
        ));
    };
    Ok(Some(Pinned {
        version: version.clone(),
        asset: asset.clone(),
        kind: release.kind,
        known: Some(release),
    }))
}

/// The installed path of the pinned release, downloading it first when the
/// config allows.
fn provision(tool: &Tool, config: &ToolConfig, pin: &Pinned<'_>) -> Result<PathBuf, String> {
    let path = install::destination(&tool.name, &pin.version, &pin.asset)
        .map_err(|error| format!("locating poly's tool cache failed: {error:#}"))?;
    if install::is_installed(&path, &pin.asset) {
        return Ok(path);
    }
    if config.install != InstallPolicy::Auto {
        return Err(format!(
            "release {} is not in poly's tool cache and `install` is not \"auto\"",
            pin.version
        ));
    }
    install::ensure(&path, &pin.asset, pin.kind)
        .map(|()| path)
        .map_err(|error| format!("installing release {} failed: {error:#}", pin.version))
}

/// The message for a `version` pin poly cannot honour. A pin that falls back to
/// whatever `PATH` holds would lint with a tool the config did not ask for.
fn unmet_pin(tool: &Tool, pin: &Pinned<'_>, reason: &str) -> String {
    format!(
        "tool `{name}` is pinned to version {version} in [tools.{name}] but {reason}; \
         poly will not run a different `{binary}` from PATH instead. Set `install = \"auto\"`, \
         point `bin` at the pinned build, or remove `version`",
        name = tool.name,
        version = pin.version,
        binary = tool.binary,
    )
}

fn missing_message(tool: &Tool, recipe: &Recipe, pinned: Option<&Pinned<'_>>) -> String {
    let hint = recipe.install.as_ref().map_or("", |install| install.hint.as_str());
    let mut message = format!(
        "tool `{}` is enabled in [tools.{}] but `{}` was not found on PATH",
        tool.name, tool.name, tool.binary
    );
    if !hint.is_empty() {
        message.push_str(&format!("; install it with: {hint}"));
    }
    if let Some(pin) = pinned {
        let _ = pin.known;
        message.push_str(&format!(
            "; or set `install = \"auto\"` and poly will download the pinned {} release into its cache",
            pin.version
        ));
    }
    message
}
