//! `poly mcp`: serve MCP over stdio.
//!
//! The server is the `mcp` frontend component, not part of the core binary. This command makes
//! the component available (downloading it unless `--offline` or `[components] install = "never"`
//! forbids it), then hands it the process's stdio. Everything the server does for its client goes
//! back through [`Services`], the host services registered with the component ABI.
//!
//! A build with the `bundled-mcp` feature links the server in-process instead and never touches
//! the component machinery; it is in the default features, so release binaries ship `poly mcp`
//! in-process until the component is published; a `--no-default-features` core build takes this path.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use poly_host_services::Services;

use crate::components_cmd::HostComponents;

/// The component the server ships as.
#[cfg(not(feature = "bundled-mcp"))]
const COMPONENT: &str = "mcp";

/// The host services every frontend gets.
fn services() -> Arc<Services> {
    Arc::new(Services::new().with_components(Arc::new(HostComponents)))
}

/// Run `poly mcp`; `config` is the global `--config`, the fallback for requests naming none.
#[must_use]
pub fn run_mcp(config: Option<PathBuf>) -> ExitCode {
    match serve(config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("poly mcp: {error:#}");
            ExitCode::from(2)
        }
    }
}

#[cfg(feature = "bundled-mcp")]
fn serve(config: Option<PathBuf>) -> anyhow::Result<()> {
    poly_mcp::serve(services(), config)
}

#[cfg(not(feature = "bundled-mcp"))]
fn serve(config: Option<PathBuf>) -> anyhow::Result<()> {
    component::serve(services(), config)
}

#[cfg(not(feature = "bundled-mcp"))]
mod component {
    use std::path::PathBuf;
    use std::sync::Arc;

    use alef_component_runtime::HostServices;
    use anyhow::Context;
    use poly_component_host::{Availability, ComponentHost};
    use poly_config::v2::ComponentInstallPolicy;
    use poly_core::components::host_options;
    use poly_frontend_api::ServiceCaller;
    use poly_frontend_api::wire::FrontendArgs;
    use poly_host_services::Services;

    use super::COMPONENT;

    /// Registers [`Services`] as the component ABI's host service table.
    struct Bridge(Arc<Services>);

    impl HostServices for Bridge {
        fn call(&self, service: &str, request: &[u8]) -> Result<Vec<u8>, String> {
            self.0.call(service, request)
        }

        fn notify(&self, service: &str, payload: &[u8]) {
            tracing::debug!(service, bytes = payload.len(), "frontend notification");
        }
    }

    /// The `[components]` settings of the config in effect, from the global `--config` or the
    /// working directory. Remote `extends` bases resolve as they do for every other command.
    fn components_config(config: Option<&std::path::Path>) -> anyhow::Result<poly_config::v2::ComponentsConfig> {
        let resolver = crate::config_sources::resolver()?;
        let loaded = match config {
            Some(file) => poly_config::PolyConfig::load_file_with(file, &resolver)?,
            None => {
                let cwd = std::env::current_dir().context("failed to resolve the working directory")?;
                poly_config::PolyConfig::load_with(&cwd, &resolver)?
            }
        };
        Ok(loaded.components)
    }

    pub(super) fn serve(services: Arc<Services>, config: Option<PathBuf>) -> anyhow::Result<()> {
        // A broken config must not kill the server before the handshake: requests carry their own
        // config and report its errors per call, so fall back to the default install policy here.
        let policy = components_config(config.as_deref()).unwrap_or_else(|error| {
            eprintln!("poly mcp: cannot read the component policy, using the defaults: {error:#}");
            poly_config::v2::ComponentsConfig::default()
        });
        anyhow::ensure!(
            !policy.disable.iter().any(|name| name == COMPONENT),
            "the `{COMPONENT}` component is disabled by `[components] disable`"
        );
        let mut options = host_options().with_context(|| {
            format!(
                "`poly mcp` needs the `{COMPONENT}` component, and this build has none to fetch; \
                 install a poly release that ships it, or build with the `bundled-mcp` feature"
            )
        })?;
        options.offline = crate::offline();
        options.gate_loads = true;
        options.services = Some(Arc::new(Bridge(services)));
        let host = ComponentHost::new(options)?;

        let allow_install = policy.install == ComponentInstallPolicy::Auto;
        match host.ensure_frontend(COMPONENT, allow_install) {
            Availability::Present => {}
            Availability::InstalledNow => eprintln!("poly mcp: installed the `{COMPONENT}` component"),
            Availability::Unavailable(unavailable) => anyhow::bail!("{}", unavailable.reason(COMPONENT)),
        }
        let args = serde_json::to_vec(&FrontendArgs {
            config: config.map(|path| path.display().to_string()),
        })?;
        host.run_frontend(COMPONENT, &args)
    }
}
