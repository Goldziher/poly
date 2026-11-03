//! The component producer: adapts a set of [`Engine`]s to the [`PolyComponent`] contract.
//!
//! The producer is what an engine component links: it owns the hosted engines, decodes each
//! protocol request, dispatches it to the named engine, and encodes the answer. A panic or error in
//! an engine never crosses the boundary as such; it becomes a [`WireError`]
//! in the [`Response`]. `tracing` events an engine emits during a call travel back in
//! [`Response::logs`](poly_component_protocol::Response::logs).

mod describe;
mod dispatch;
mod errors;
mod files;
mod language;
mod logs;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use poly_component_protocol::{
    ConfigId, Response, WireError, WireErrorKind, WireLogLevel, decode_request, encode_describe, encode_response,
};
use poly_engine_api::config::EngineConfig;
use poly_engine_api::engine::Engine;

pub use poly_engine_api::component_wire::{engine_config_from_wire, engine_config_to_wire};

use crate::{ComponentCallError, PolyComponent};

/// What the host passed in `Request::Init`.
///
/// Recorded, not acted on: the producer runs engines on the calling thread and builds no thread
/// pool of its own, so the host's worker pool is the only parallelism.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitState {
    /// Host (poly) version.
    pub host_version: String,
    /// Worker threads the component may use.
    pub threads: u32,
    /// Stack size in bytes of the host's worker threads.
    pub stack_size: u64,
    /// Most verbose level the host wants in `Response::logs`.
    pub max_log_level: WireLogLevel,
}

/// A [`PolyComponent`] over a fixed set of engines.
pub struct Producer {
    engines: Vec<Box<dyn Engine>>,
    component_version: String,
    configs: RwLock<HashMap<ConfigId, Arc<EngineConfig>>>,
    init: OnceLock<InitState>,
}

impl Producer {
    /// A producer hosting `engines`, reporting `component_version` from `describe()`.
    pub fn new(component_version: impl Into<String>, engines: Vec<Box<dyn Engine>>) -> Self {
        Self {
            engines,
            component_version: component_version.into(),
            configs: RwLock::new(HashMap::new()),
            init: OnceLock::new(),
        }
    }

    /// What the host passed to `Init`; `None` before the first `Init` call.
    pub fn init_state(&self) -> Option<&InitState> {
        self.init.get()
    }

    fn register(&self, id: ConfigId, config: EngineConfig) {
        self.configs
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, Arc::new(config));
    }

    fn config(&self, id: ConfigId) -> Result<Arc<EngineConfig>, WireError> {
        self.configs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .cloned()
            .ok_or_else(|| WireError {
                kind: WireErrorKind::UnknownConfig,
                message: format!("config {id} was never registered"),
                chain: Vec::new(),
            })
    }

    /// The engine called `name`. Several engines may share a name across languages (`biome` for
    /// CSS and GraphQL), so `language` picks the one serving it, then a cross-cutting one.
    fn engine(&self, name: &str, language: Option<&str>) -> Result<&dyn Engine, WireError> {
        let named: Vec<&dyn Engine> = self
            .engines
            .iter()
            .map(|engine| engine.as_ref())
            .filter(|engine| engine.name() == name)
            .collect();
        let Some(first) = named.first() else {
            return Err(WireError {
                kind: WireErrorKind::UnknownEngine,
                message: format!("this component hosts no engine named `{name}`"),
                chain: Vec::new(),
            });
        };
        if let Some(language) = language
            && let Some(engine) = named
                .iter()
                .find(|engine| engine.languages().iter().any(|l| l.id() == language))
                .or_else(|| named.iter().find(|engine| engine.languages().is_empty()))
        {
            return Ok(*engine);
        }
        Ok(*first)
    }
}

impl PolyComponent for Producer {
    fn describe(&self) -> Result<Vec<u8>, ComponentCallError> {
        let describe = describe::describe(&self.component_version, &self.engines);
        encode_describe(&describe).map_err(|e| ComponentCallError::with_source("failed to encode describe()", e))
    }

    fn call(&self, header: &[u8], body: &[u8]) -> Result<Vec<u8>, ComponentCallError> {
        let capture = logs::LogCapture::default();
        let max_level = self.init.get().map_or(WireLogLevel::Info, |init| init.max_log_level);
        let result = capture.run(max_level, || {
            let request =
                decode_request(header).map_err(|e| errors::protocol(format!("undecodable request header: {e}")))?;
            self.dispatch(request, body)
        });
        let response = Response {
            result,
            logs: capture.take(),
        };
        encode_response(&response).map_err(|e| ComponentCallError::with_source("failed to encode response", e))
    }
}
