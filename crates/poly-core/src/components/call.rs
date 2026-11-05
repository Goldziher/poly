//! One request to the component that hosts a given engine, outside a lint or format run.
//!
//! `poly rules` and the MCP `rules` tool need the ast-grep rule inventory, which only the
//! `astgrep` component holds in a core build. This is the one entry point that makes such a
//! component usable on demand, with the same install policy a run applies, and calls it.

use anyhow::Context;
use poly_component_host::{Availability, ComponentHost};
use poly_component_index::ComponentIndex;
use poly_component_protocol::{Payload, Request};
use poly_config::network;
use poly_config::v2::ComponentInstallPolicy;

use super::preplan::expected_describe;
use super::seam::ComponentInjection;
use super::select::{default_cache_root, host_options};
use crate::config::Config;

/// Send `request` to the component hosting `engine` and return its payload.
///
/// The component is installed first when it is not cached, unless the process is offline
/// (`--offline`, `POLY_OFFLINE`) or `[components] install = "never"`. There is no skip policy
/// here: the caller asked for this component's answer, so an unavailable component is an error
/// whatever `on_missing` says. `injection` replaces the embedded index and lock (tests).
///
/// # Errors
///
/// When no component hosts `engine`, `[components] disable` names it, it cannot be made
/// available, or the call fails.
pub fn call_engine_component(
    injection: Option<&ComponentInjection>,
    config: &Config,
    engine: &str,
    request: &Request,
) -> anyhow::Result<Payload> {
    let index: &ComponentIndex = match injection {
        Some(injected) => &injected.index,
        None => poly_component_index::index(),
    };
    let component = index
        .component_for_engine(engine)
        .with_context(|| format!("no engine component hosts `{engine}`"))?;
    let name = component.name.as_str();
    let disabled = config
        .components
        .disable
        .iter()
        .filter_map(|entry| index.expand_preset(entry))
        .flatten()
        .any(|disabled| disabled.name == component.name);
    anyhow::ensure!(
        !disabled,
        "the `{name}` component is disabled by `[components] disable`"
    );

    let host = match injection {
        Some(injected) => std::sync::Arc::clone(&injected.host),
        None => {
            let mut options = host_options()
                .with_context(|| format!("this build embeds no component lock, so `{name}` cannot be fetched"))?;
            options.offline = network::offline();
            options.cache_root = default_cache_root();
            std::sync::Arc::new(ComponentHost::new(options)?)
        }
    };
    let allow_install = config.components.install == ComponentInstallPolicy::Auto;
    match host.ensure(name, allow_install) {
        Availability::Present => {}
        Availability::InstalledNow => tracing::info!(component = name, "installed component"),
        Availability::Unavailable(unavailable) => anyhow::bail!("{}", unavailable.reason(name)),
    }
    let slot = host.slot(name, Some(expected_describe(component)))?;
    slot.connection()?.call_plain(engine, request, &[])
}
