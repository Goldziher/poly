//! Service names, request and reply bodies, and the caller seam.

use std::collections::BTreeMap;

use anyhow::Context;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dto::ComponentSummary;

/// Names of the services a host answers.
pub mod service {
    /// Lint paths ([`super::PathsRequest`] -> [`super::RunReply`]).
    pub const LINT: &str = "lint.run";
    /// Check or apply formatting ([`super::PathsRequest`] -> [`super::RunReply`]).
    pub const FORMAT: &str = "format.run";
    /// The whole-project lint phase ([`super::WorkspaceRequest`] -> `WorkspaceReport`).
    pub const WORKSPACE_LINT: &str = "workspace.lint";
    /// Result-cache footprint (no request -> `CacheStatsReport`).
    pub const CACHE_STATS: &str = "cache.stats";
    /// Empty the result cache (no request -> `CacheCleanReport`).
    pub const CACHE_CLEAN: &str = "cache.clean";
    /// The effective config ([`super::ConfigRequest`] -> `ConfigShowReport`).
    pub const CONFIG_SHOW: &str = "config.show";
    /// The ast-grep rule inventory ([`super::RulesRequest`] -> `RulesReport`).
    pub const RULES: &str = "rules.report";
    /// The downloadable components (no request -> `Vec<ComponentSummary>`).
    pub const COMPONENTS: &str = "components.list";
    /// What the components need and where each stands ([`super::ComponentsStatusRequest`] -> `ComponentsStatus`).
    pub const COMPONENTS_STATUS: &str = "components.status";
    /// Download components ([`super::ComponentsInstallRequest`] -> `ComponentActions`).
    pub const COMPONENTS_INSTALL: &str = "components.install";
    /// Remove components ([`super::ComponentsUninstallRequest`] -> `ComponentActions`).
    pub const COMPONENTS_UNINSTALL: &str = "components.uninstall";
    /// Compiled-in engine versions (no request -> [`super::Engines`]).
    pub const ENGINES: &str = "engines.versions";
    /// JSON Schemas of the lint and format documents (no request -> [`super::Schemas`]).
    pub const SCHEMAS: &str = "schemas.output";
}

/// What a frontend receives from the host when it is started (`PolyFrontend::run`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FrontendArgs {
    /// Config file pinned on the command line, used for every request that names none.
    #[serde(default)]
    pub config: Option<String>,
}

/// Whether a run may download a missing component.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum InstallMode {
    /// Follow `[components] install` in the config.
    #[default]
    Config,
    /// Download a missing component, whatever the config says.
    Auto,
    /// Download nothing; a missing component is skipped.
    Never,
}

/// Request body of `lint.run` and `format.run`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PathsRequest {
    /// Files or directories; empty means the current directory.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Gitignore-style globs to exclude.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Config file to use instead of discovery.
    #[serde(default)]
    pub config: Option<String>,
    /// Apply fixes (`lint.run`) or write files (`format.run`).
    #[serde(default)]
    pub apply: bool,
    /// Whether a missing component may be downloaded.
    #[serde(default)]
    pub install: InstallMode,
}

/// Request body of `components.status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComponentsStatusRequest {
    /// Files or directories to inspect; empty lists every component without judging need.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Config file to use instead of discovery.
    #[serde(default)]
    pub config: Option<String>,
    /// The install policy `would_download` is judged under.
    #[serde(default)]
    pub install: InstallMode,
}

/// Request body of `components.install`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComponentsInstallRequest {
    /// Component names or presets.
    #[serde(default)]
    pub names: Vec<String>,
    /// Languages whose components to install (ids or preset aliases such as `python`).
    #[serde(default)]
    pub languages: Vec<String>,
    /// Files or directories; installs every component they need.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Config file to use instead of discovery (for `paths`).
    #[serde(default)]
    pub config: Option<String>,
    /// Report what would be downloaded and download nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// Request body of `components.uninstall`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComponentsUninstallRequest {
    /// Component names or presets.
    #[serde(default)]
    pub names: Vec<String>,
    /// Also remove cached archives the lock no longer references and nothing used recently.
    #[serde(default)]
    pub stale: bool,
}

/// Request body of `config.show`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigRequest {
    /// Config file to use instead of discovery.
    #[serde(default)]
    pub config: Option<String>,
}

/// Request body of `rules.report`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RulesRequest {
    /// User rule directories; empty means `[rules] dirs`.
    #[serde(default)]
    pub dirs: Vec<String>,
    /// Config file to use instead of discovery.
    #[serde(default)]
    pub config: Option<String>,
    /// Also run the rule tests.
    #[serde(default)]
    pub test: bool,
}

/// Request body of `workspace.lint`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceRequest {
    /// Config file to use instead of discovery.
    #[serde(default)]
    pub config: Option<String>,
    /// Run the tools in fix mode.
    #[serde(default)]
    pub fix: bool,
    /// Concurrency override.
    #[serde(default)]
    pub jobs: Option<usize>,
    /// Bypass the result cache.
    #[serde(default)]
    pub no_cache: bool,
}

/// Reply of `lint.run` and `format.run`: the run rendered three ways, so the frontend adds
/// nothing and cannot disagree with the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReply {
    /// The `--format json` document as a value (the tool's structured content).
    pub document: Value,
    /// The document as the CLI prints it with `--format json`.
    pub json: String,
    /// The document as the CLI prints it with `--format toon`.
    pub toon: String,
    /// Whether the run failed on a file or a component blocks it (the tool's `isError`).
    pub is_error: bool,
}

/// Reply of `engines.versions`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Engines {
    /// Every compiled-in backend and the upstream version it wraps.
    pub engines: BTreeMap<String, String>,
    /// `bundled`, `core` or `partial`; `unknown` for a host with no component manager.
    #[serde(default = "unknown_flavor")]
    pub flavor: String,
    /// Digest of the component lock the host embeds, or none.
    #[serde(default)]
    pub components_lock: Option<String>,
}

fn unknown_flavor() -> String {
    "unknown".to_owned()
}

/// Reply of `schemas.output`: draft 2020-12 root schemas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schemas {
    /// Schema of the lint document.
    pub lint: Value,
    /// Schema of the format document.
    pub format: Value,
}

/// Reply of `components.list`.
pub type Components = Vec<ComponentSummary>;

/// One way to reach the host's services.
///
/// `Send + Sync` because a frontend calls from blocking worker threads.
pub trait ServiceCaller: Send + Sync + 'static {
    /// Send `request` to `service` and return the response bytes; the error is the host's message.
    ///
    /// # Errors
    ///
    /// When the host has no such service or the service failed.
    fn call(&self, service: &str, request: &[u8]) -> Result<Vec<u8>, String>;
}

/// Encode `request`, call `service` and decode the reply.
///
/// # Errors
///
/// When a body does not encode or decode, or the host reports a failure.
pub fn call<Req: Serialize, Rep: DeserializeOwned>(
    caller: &dyn ServiceCaller,
    service: &str,
    request: &Req,
) -> anyhow::Result<Rep> {
    let bytes = serde_json::to_vec(request).with_context(|| format!("cannot encode the `{service}` request"))?;
    let reply = caller
        .call(service, &bytes)
        .map_err(|message| anyhow::anyhow!("host service `{service}` failed: {message}"))?;
    serde_json::from_slice(&reply).with_context(|| format!("cannot decode the `{service}` reply"))
}
