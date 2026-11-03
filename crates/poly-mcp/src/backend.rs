//! The server's only way to the engines: calls to the host's services.
//!
//! [`Backend`] wraps a [`ServiceCaller`] and gives each service a typed method. It carries no
//! engine: whether the caller is the component ABI's service table, an in-process host or a test
//! fake is invisible to the server.

use std::sync::Arc;

use poly_frontend_api::dto::{
    CacheCleanReport, CacheStatsReport, ComponentActions, ComponentSummary, ComponentsStatus, ConfigShowReport,
    RulesReport, WorkspaceReport,
};
use poly_frontend_api::wire::{
    ComponentsInstallRequest, ComponentsStatusRequest, ComponentsUninstallRequest, ConfigRequest, Engines,
    PathsRequest, RulesRequest, RunReply, Schemas, WorkspaceRequest, service,
};
use poly_frontend_api::{ServiceCaller, call};

/// Typed access to the host's services. Cheap to clone: it shares one caller.
#[derive(Clone)]
pub struct Backend {
    caller: Arc<dyn ServiceCaller>,
}

impl Backend {
    /// A backend over `caller`.
    #[must_use]
    pub fn new(caller: Arc<dyn ServiceCaller>) -> Self {
        Self { caller }
    }

    /// Lint, applying fixes when `request.apply` is set.
    ///
    /// # Errors
    ///
    /// When the host fails the run or the reply does not decode.
    pub fn lint(&self, request: &PathsRequest) -> anyhow::Result<RunReply> {
        call(&*self.caller, service::LINT, request)
    }

    /// Check formatting, or write files when `request.apply` is set.
    ///
    /// # Errors
    ///
    /// As [`Backend::lint`].
    pub fn format(&self, request: &PathsRequest) -> anyhow::Result<RunReply> {
        call(&*self.caller, service::FORMAT, request)
    }

    /// Run the whole-project lint phase.
    ///
    /// # Errors
    ///
    /// As [`Backend::lint`].
    pub fn workspace_lint(&self, request: &WorkspaceRequest) -> anyhow::Result<WorkspaceReport> {
        call(&*self.caller, service::WORKSPACE_LINT, request)
    }

    /// The result-cache footprint.
    ///
    /// # Errors
    ///
    /// As [`Backend::lint`].
    pub fn cache_stats(&self) -> anyhow::Result<CacheStatsReport> {
        call(&*self.caller, service::CACHE_STATS, &())
    }

    /// Empty the result cache.
    ///
    /// # Errors
    ///
    /// As [`Backend::lint`].
    pub fn cache_clean(&self) -> anyhow::Result<CacheCleanReport> {
        call(&*self.caller, service::CACHE_CLEAN, &())
    }

    /// The effective config.
    ///
    /// # Errors
    ///
    /// As [`Backend::lint`].
    pub fn config_show(&self, request: &ConfigRequest) -> anyhow::Result<ConfigShowReport> {
        call(&*self.caller, service::CONFIG_SHOW, request)
    }

    /// The rule inventory.
    ///
    /// # Errors
    ///
    /// As [`Backend::lint`].
    pub fn rules(&self, request: &RulesRequest) -> anyhow::Result<RulesReport> {
        call(&*self.caller, service::RULES, request)
    }

    /// The downloadable components and where each stands.
    ///
    /// # Errors
    ///
    /// As [`Backend::lint`].
    pub fn components(&self) -> anyhow::Result<Vec<ComponentSummary>> {
        call(&*self.caller, service::COMPONENTS, &())
    }

    /// What each component needs and where it stands. Never downloads.
    ///
    /// # Errors
    ///
    /// As [`Backend::lint`].
    pub fn components_status(&self, request: &ComponentsStatusRequest) -> anyhow::Result<ComponentsStatus> {
        call(&*self.caller, service::COMPONENTS_STATUS, request)
    }

    /// Download components, or report what would be downloaded when `request.dry_run` is set.
    ///
    /// # Errors
    ///
    /// As [`Backend::lint`].
    pub fn components_install(&self, request: &ComponentsInstallRequest) -> anyhow::Result<ComponentActions> {
        call(&*self.caller, service::COMPONENTS_INSTALL, request)
    }

    /// Remove components the process has not loaded.
    ///
    /// # Errors
    ///
    /// As [`Backend::lint`].
    pub fn components_uninstall(&self, request: &ComponentsUninstallRequest) -> anyhow::Result<ComponentActions> {
        call(&*self.caller, service::COMPONENTS_UNINSTALL, request)
    }

    /// The host's compiled-in engine versions, flavor and component lock.
    ///
    /// # Errors
    ///
    /// As [`Backend::lint`].
    pub fn engines(&self) -> anyhow::Result<Engines> {
        call(&*self.caller, service::ENGINES, &())
    }

    /// The lint and format document schemas, for the tools' output schemas.
    ///
    /// # Errors
    ///
    /// As [`Backend::lint`].
    pub fn schemas(&self) -> anyhow::Result<Schemas> {
        call(&*self.caller, service::SCHEMAS, &())
    }
}
