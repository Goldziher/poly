//! [`ComponentHost`]: the manager, the connector and one slot per component.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use alef_component_abi::AlefHostApiV1;
use alef_component_runtime::{
    ComponentLock, ComponentManager, ComponentStatus, DownloadOptions, ProgressCallback, target::host_target,
};
use anyhow::Context;
use poly_component_protocol::{Describe, WireEngineDesc};
use poly_config::network::{component_cache_dir, component_url};

use crate::engine::ComponentEngine;
use crate::slot::{ComponentSlot, InitParams};
use crate::transport::{AlefConnector, TransportConnector};

/// Digest used for the version of an engine whose component has no lock entry for this target.
const NO_LOCK_DIGEST: &str = "0000000000000000";

/// What [`ComponentHost::new`] needs.
pub struct HostOptions {
    /// The embedded component lock, as JSON.
    pub lock_json: String,
    /// Cache root for downloaded components; `POLY_COMPONENT_CACHE` overrides it.
    pub cache_root: PathBuf,
    /// Never touch the network: a component missing from the cache fails to load.
    pub offline: bool,
    /// Target triple to select lock entries for; `None` is the host's own
    /// ([`alef_component_runtime::target::host_target`]).
    pub target: Option<String>,
    /// Called with `(bytes_done, bytes_total)` while an archive downloads.
    pub progress: Option<ProgressCallback>,
    /// Settings sent to each component with `Init`.
    pub init: InitParams,
}

impl HostOptions {
    /// Options with defaults for everything but the lock and the cache root.
    #[must_use]
    pub fn new(lock_json: impl Into<String>, cache_root: impl Into<PathBuf>) -> Self {
        Self {
            lock_json: lock_json.into(),
            cache_root: cache_root.into(),
            offline: false,
            target: None,
            progress: None,
            init: InitParams::default(),
        }
    }
}

/// Loads components lazily and hands out their engines.
///
/// A component is loaded the first time one of its engines needs it, never unloaded, and a failed
/// load is remembered for the rest of the run.
pub struct ComponentHost {
    manager: Arc<ComponentManager>,
    connector: Arc<dyn TransportConnector>,
    init: InitParams,
    slots: Mutex<HashMap<String, Arc<ComponentSlot>>>,
}

impl ComponentHost {
    /// A host over the embedded lock, connecting through alef ([`AlefConnector`]).
    ///
    /// Downloads honour `POLY_COMPONENT_MIRROR` (the origin of each lock URL is replaced, the
    /// path kept; integrity is still checked against the lock) and `POLY_COMPONENT_CACHE`.
    /// Installed libraries are trusted through their verified stamp.
    ///
    /// # Errors
    ///
    /// When the lock is not valid JSON, has an unsupported schema or repeats an entry, or when
    /// no target is given and the host has none.
    pub fn new(options: HostOptions) -> anyhow::Result<Self> {
        let lock: ComponentLock =
            serde_json::from_str(&options.lock_json).context("the embedded component lock is not valid")?;
        let target = match options.target {
            Some(target) => target,
            None => host_target()
                .context("no components are published for this platform")?
                .to_owned(),
        };
        let cache_root = component_cache_dir().unwrap_or(options.cache_root);
        let mut downloads = DownloadOptions::default();
        if let Some(progress) = options.progress {
            downloads = downloads.with_progress(move |done, total| progress(done, total));
        }
        let manager = ComponentManager::from_lock(lock, cache_root, target, AlefHostApiV1::EMPTY)
            .context("the embedded component lock was rejected")?
            .offline(options.offline)
            .with_download_options(downloads)
            .with_url_override(|entry| component_url(&entry.url))
            .with_verified_stamp(true);
        Ok(Self {
            manager: Arc::new(manager),
            connector: Arc::new(AlefConnector),
            init: options.init,
            slots: Mutex::new(HashMap::new()),
        })
    }

    /// Replace how loaded components are reached (tests, or the generated alef proxy).
    #[must_use]
    pub fn with_connector(mut self, connector: Arc<dyn TransportConnector>) -> Self {
        self.connector = connector;
        self
    }

    /// The underlying manager, for cache maintenance and status.
    #[must_use]
    pub fn manager(&self) -> &Arc<ComponentManager> {
        &self.manager
    }

    /// Where `component_id` stands in the cache, without loading it.
    ///
    /// # Errors
    ///
    /// When the lock has no entry for it on this target.
    pub fn status(&self, component_id: &str) -> anyhow::Result<ComponentStatus> {
        Ok(self.manager.status(component_id)?)
    }

    /// The lock digest of `component_id` for this target, if the lock has one.
    #[must_use]
    pub fn lock_sha256(&self, component_id: &str) -> Option<String> {
        self.manager
            .lock_entry(component_id)
            .ok()
            .map(|entry| entry.sha256.clone())
    }

    /// The component's slot, created on first request. `expected` (the index entry set) is used
    /// only by the call that creates the slot.
    ///
    /// # Errors
    ///
    /// When the lock has no entry for `component_id` on this target.
    pub fn slot(&self, component_id: &str, expected: Option<Describe>) -> anyhow::Result<Arc<ComponentSlot>> {
        self.manager
            .lock_entry(component_id)
            .with_context(|| format!("component `{component_id}` is not in the lock"))?;
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = slots.get(component_id) {
            return Ok(Arc::clone(slot));
        }
        let manager = Arc::clone(&self.manager);
        let connector = Arc::clone(&self.connector);
        let id = component_id.to_owned();
        let slot = ComponentSlot::lazy(component_id, expected, self.init.clone(), move || {
            connector.connect(&manager, &id)
        });
        slots.insert(component_id.to_owned(), Arc::clone(&slot));
        Ok(slot)
    }

    /// The proxy for one engine of `component_id`; its component loads on first use.
    ///
    /// # Errors
    ///
    /// When the component is not in the lock or `desc` is malformed.
    pub fn engine(
        &self,
        component_id: &str,
        desc: &WireEngineDesc,
        expected: Option<Describe>,
    ) -> anyhow::Result<ComponentEngine> {
        let slot = self.slot(component_id, expected)?;
        let sha = self.lock_sha256(component_id).context("component has no lock entry")?;
        ComponentEngine::new(component_id, desc, &sha, slot)
    }

    /// The stand-in for an engine whose component cannot be used; see
    /// [`ComponentEngine::unavailable`]. A component with no lock entry on this target reports
    /// an all-zero digest in its version.
    ///
    /// # Errors
    ///
    /// When `desc` is malformed.
    pub fn unavailable_engine(
        &self,
        component_id: &str,
        desc: &WireEngineDesc,
        reason: impl Into<String>,
    ) -> anyhow::Result<ComponentEngine> {
        let sha = self
            .lock_sha256(component_id)
            .unwrap_or_else(|| NO_LOCK_DIGEST.to_owned());
        ComponentEngine::unavailable(component_id, desc, &sha, reason)
    }
}
