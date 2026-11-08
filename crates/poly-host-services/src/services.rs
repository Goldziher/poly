//! [`Services`]: decode a request, run the operation, encode the reply.

use std::sync::Arc;

use anyhow::Context;
use poly_core::report::{FormatDocument, LintDocument};
use poly_core::{FormatRun, LintRun};
use poly_frontend_api::ServiceCaller;
use poly_frontend_api::dto::{ComponentActions, ComponentSummary, ComponentsStatus};
use poly_frontend_api::wire::{
    ComponentsInstallRequest, ComponentsStatusRequest, ComponentsUninstallRequest, ConfigRequest, Engines,
    PathsRequest, RulesRequest, RunReply, Schemas, WorkspaceRequest, service,
};
use schemars::generate::SchemaSettings;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::ops;

/// Lists the components for the `components.list` service. The core binary supplies it, since
/// what is installed is the component host's knowledge, not this crate's.
pub trait ComponentsProvider: Send + Sync {
    /// The downloadable components and where each stands.
    ///
    /// # Errors
    ///
    /// When the component state cannot be read.
    fn components(&self) -> anyhow::Result<Vec<ComponentSummary>>;

    /// The build's flavor (`bundled`, `core` or `partial`) and the digest of its component lock,
    /// for the identity every result carries. Never downloads.
    fn identity(&self) -> (String, Option<String>);

    /// Where each component stands and which ones `request.paths` need. Never downloads.
    ///
    /// # Errors
    ///
    /// When the paths or their config cannot be resolved.
    fn status(&self, request: &ComponentsStatusRequest) -> anyhow::Result<ComponentsStatus>;

    /// Download the components `request` selects, or report what would be downloaded.
    ///
    /// # Errors
    ///
    /// When a name is neither a component nor a preset, or no component lock is available.
    fn install(&self, request: &ComponentsInstallRequest) -> anyhow::Result<ComponentActions>;

    /// Remove the components `request` selects, refusing any this process has loaded.
    ///
    /// # Errors
    ///
    /// When a name is neither a component nor a preset, or no component lock is available.
    fn uninstall(&self, request: &ComponentsUninstallRequest) -> anyhow::Result<ComponentActions>;
}

/// The host side of the frontend service table.
#[derive(Clone, Default)]
pub struct Services {
    components: Option<Arc<dyn ComponentsProvider>>,
}

fn no_manager<T>() -> anyhow::Result<T> {
    anyhow::bail!("this host has no component manager")
}

impl Services {
    /// Services with no component listing: `components.list` answers an empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer `components.list` from `provider`.
    #[must_use]
    pub fn with_components(mut self, provider: Arc<dyn ComponentsProvider>) -> Self {
        self.components = Some(provider);
        self
    }

    /// Run `service` on `request` and return the encoded reply.
    ///
    /// # Errors
    ///
    /// When the service is unknown, the request does not decode, or the operation fails. The
    /// message is what the frontend shows its client.
    pub fn dispatch(&self, service_name: &str, request: &[u8]) -> anyhow::Result<Vec<u8>> {
        match service_name {
            service::LINT => {
                let req: PathsRequest = decode(request)?;
                let run = ops::lint_run_with(&req.paths, &req.exclude, req.config.as_deref(), req.apply, req.install)?;
                encode(&lint_reply(&run)?)
            }
            service::FORMAT => {
                let req: PathsRequest = decode(request)?;
                let run =
                    ops::format_run_with(&req.paths, &req.exclude, req.config.as_deref(), req.apply, req.install)?;
                encode(&format_reply(&run)?)
            }
            service::WORKSPACE_LINT => {
                let req: WorkspaceRequest = decode(request)?;
                encode(&ops::workspace_lint(
                    req.config.as_deref(),
                    req.fix,
                    req.jobs,
                    req.no_cache,
                )?)
            }
            service::CACHE_STATS => encode(&ops::cache_stats()?),
            service::CACHE_CLEAN => encode(&ops::cache_clean()?),
            service::CONFIG_SHOW => {
                let req: ConfigRequest = decode(request)?;
                encode(&ops::config_show(req.config.as_deref())?)
            }
            service::RULES => {
                let req: RulesRequest = decode(request)?;
                encode(&ops::rules_report(&req.dirs, req.config.as_deref(), req.test)?)
            }
            service::COMPONENTS => {
                let listed = match &self.components {
                    Some(provider) => provider.components()?,
                    None => Vec::new(),
                };
                encode(&listed)
            }
            service::COMPONENTS_STATUS => {
                let req: ComponentsStatusRequest = decode(request)?;
                match &self.components {
                    Some(provider) => encode(&provider.status(&req)?),
                    None => encode(&empty_status()),
                }
            }
            service::COMPONENTS_INSTALL => {
                let req: ComponentsInstallRequest = decode(request)?;
                match &self.components {
                    Some(provider) => encode(&provider.install(&req)?),
                    None => no_manager(),
                }
            }
            service::COMPONENTS_UNINSTALL => {
                let req: ComponentsUninstallRequest = decode(request)?;
                match &self.components {
                    Some(provider) => encode(&provider.uninstall(&req)?),
                    None => no_manager(),
                }
            }
            service::ENGINES => {
                let (flavor, components_lock) = match &self.components {
                    Some(provider) => provider.identity(),
                    None => ("unknown".to_owned(), None),
                };
                encode(&Engines {
                    engines: poly_core::engine_versions()
                        .iter()
                        .map(|(name, version)| ((*name).to_owned(), version.clone()))
                        .collect(),
                    flavor,
                    components_lock,
                })
            }
            service::SCHEMAS => encode(&Schemas {
                lint: root_schema::<LintDocument>()?,
                format: root_schema::<FormatDocument>()?,
            }),
            other => anyhow::bail!("unknown host service `{other}`"),
        }
    }
}

impl ServiceCaller for Services {
    fn call(&self, service: &str, request: &[u8]) -> Result<Vec<u8>, String> {
        self.dispatch(service, request).map_err(|error| format!("{error:#}"))
    }
}

/// The status of a host with no component manager: nothing listed, nothing needed.
fn empty_status() -> ComponentsStatus {
    ComponentsStatus {
        flavor: "unknown".to_owned(),
        offline: false,
        cache_root: String::new(),
        components_lock: None,
        lock_error: Some("this host has no component manager".to_owned()),
        components: Vec::new(),
        missing: None,
        would_download: Vec::new(),
    }
}

fn decode<T: DeserializeOwned>(request: &[u8]) -> anyhow::Result<T> {
    serde_json::from_slice(request).context("the request does not decode")
}

fn encode<T: Serialize>(reply: &T) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec(reply).context("the reply does not encode")
}

/// The draft 2020-12 root schema rmcp generates for output schemas, as a value.
fn root_schema<T: schemars::JsonSchema>() -> anyhow::Result<Value> {
    let schema = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>();
    serde_json::to_value(schema).context("the schema does not encode")
}

fn render_error(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("failed to serialize tool output: {error}")
}

/// A lint run as the three renderings and the error flag. A render failure is an error, never an
/// empty document: a caller that gets a well-formed empty one cannot tell it from a clean run.
fn lint_reply(run: &LintRun) -> anyhow::Result<RunReply> {
    let document = LintDocument::from_run(run);
    Ok(RunReply {
        is_error: !document.errors.is_empty() || document.components.blocking(),
        document: serde_json::to_value(&document).map_err(render_error)?,
        json: poly_core::report::report_lint_json_run(run).map_err(render_error)?,
        toon: poly_core::report::report_lint_toon_run(run).map_err(render_error)?,
    })
}

/// A format run, as [`lint_reply`].
fn format_reply(run: &FormatRun) -> anyhow::Result<RunReply> {
    let document = FormatDocument::from_run(run);
    Ok(RunReply {
        is_error: !document.errors.is_empty() || document.components.blocking(),
        document: serde_json::to_value(&document).map_err(render_error)?,
        json: poly_core::report::report_format_json_run(run).map_err(render_error)?,
        toon: poly_core::report::report_format_toon_run(run).map_err(render_error)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_service_is_an_error_naming_it() {
        let error = Services::new().dispatch("nope.run", b"null").unwrap_err();
        assert!(format!("{error:#}").contains("nope.run"));
    }

    #[test]
    fn a_malformed_request_is_an_error_not_a_default() {
        let error = Services::new().dispatch(service::LINT, b"{\"paths\": 3}").unwrap_err();
        assert!(format!("{error:#}").contains("does not decode"), "{error:#}");
    }

    #[test]
    fn components_list_is_empty_without_a_provider() {
        let reply = Services::new().dispatch(service::COMPONENTS, b"null").unwrap();
        assert_eq!(reply, b"[]");
    }

    #[test]
    fn components_status_is_empty_without_a_provider() {
        let reply = Services::new().dispatch(service::COMPONENTS_STATUS, b"{}").unwrap();
        let status: ComponentsStatus = serde_json::from_slice(&reply).unwrap();
        assert!(status.components.is_empty() && status.would_download.is_empty());
        assert!(status.lock_error.is_some(), "the absence is stated, not a clean report");
    }

    #[test]
    fn install_and_uninstall_without_a_provider_are_errors_not_no_ops() {
        for name in [service::COMPONENTS_INSTALL, service::COMPONENTS_UNINSTALL] {
            let error = Services::new().dispatch(name, b"{}").unwrap_err();
            assert!(
                format!("{error:#}").contains("no component manager"),
                "{name}: {error:#}"
            );
        }
    }

    #[test]
    fn the_engines_service_names_the_flavor_even_without_a_provider() {
        let reply = Services::new().dispatch(service::ENGINES, b"null").unwrap();
        let engines: Engines = serde_json::from_slice(&reply).unwrap();
        assert_eq!(engines.flavor, "unknown");
        assert_eq!(engines.components_lock, None);
    }

    /// A provider whose answers are fixed, to check what the service table does with them.
    struct Fixed;

    impl ComponentsProvider for Fixed {
        fn components(&self) -> anyhow::Result<Vec<ComponentSummary>> {
            Ok(Vec::new())
        }
        fn identity(&self) -> (String, Option<String>) {
            ("core".to_owned(), Some("abc".to_owned()))
        }
        fn status(&self, request: &ComponentsStatusRequest) -> anyhow::Result<ComponentsStatus> {
            let mut status = empty_status();
            status.would_download = request.paths.clone();
            Ok(status)
        }
        fn install(&self, request: &ComponentsInstallRequest) -> anyhow::Result<ComponentActions> {
            Ok(ComponentActions {
                command: "install".to_owned(),
                offline: false,
                ok: true,
                results: Vec::new(),
                freed_bytes: None,
                notes: request.names.clone(),
            })
        }
        fn uninstall(&self, _: &ComponentsUninstallRequest) -> anyhow::Result<ComponentActions> {
            anyhow::bail!("refused by the fixed provider")
        }
    }

    #[test]
    fn the_component_services_reach_the_provider_and_pass_its_errors_on() {
        let services = Services::new().with_components(Arc::new(Fixed));
        let engines: Engines = serde_json::from_slice(&services.dispatch(service::ENGINES, b"null").unwrap()).unwrap();
        assert_eq!(
            (engines.flavor.as_str(), engines.components_lock.as_deref()),
            ("core", Some("abc"))
        );
        let status: ComponentsStatus = serde_json::from_slice(
            &services
                .dispatch(service::COMPONENTS_STATUS, br#"{"paths":["x"]}"#)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(status.would_download, ["x"]);
        let actions: ComponentActions = serde_json::from_slice(
            &services
                .dispatch(service::COMPONENTS_INSTALL, br#"{"names":["web"]}"#)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(actions.notes, ["web"]);
        let error = services.dispatch(service::COMPONENTS_UNINSTALL, b"{}").unwrap_err();
        assert!(format!("{error:#}").contains("refused by the fixed provider"));
    }

    #[test]
    fn the_schemas_service_publishes_both_documents() {
        let reply = Services::new().dispatch(service::SCHEMAS, b"null").unwrap();
        let schemas: Schemas = serde_json::from_slice(&reply).unwrap();
        for schema in [&schemas.lint, &schemas.format] {
            assert!(schema["properties"].is_object(), "a root object schema: {schema}");
        }
    }

    // A core build has no engines linked and no component lock, so the run reports the missing
    // components as errors; the clean-summary claim holds only with every family linked.
    #[cfg(feature = "all-engines")]
    #[test]
    fn a_lint_request_runs_the_pipeline_and_reports_the_summary() {
        let _serial = crate::CONFIG_LOADING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hello\n").unwrap();
        let request = serde_json::to_vec(&PathsRequest {
            paths: vec![file.display().to_string()],
            ..PathsRequest::default()
        })
        .unwrap();
        let reply: RunReply =
            serde_json::from_slice(&Services::new().dispatch(service::LINT, &request).unwrap()).unwrap();
        assert!(reply.document["summary"].is_object(), "{}", reply.document);
        assert_eq!(reply.document["summary"]["errored"], 0, "{}", reply.document);
    }
}
