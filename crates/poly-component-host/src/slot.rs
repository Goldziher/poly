//! One component's lazily established connection: load once, remember failure, register configs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::Context;
use poly_component_protocol::{
    ConfigId, Describe, PROTOCOL_VERSION, Payload, Request, Response, WireError, WireErrorKind, WireLog, WireLogLevel,
    decode_describe, decode_response, encode_request,
};
use poly_engine_api::component_wire::engine_config_to_wire;
use poly_engine_api::config::EngineConfig;

use crate::transport::ComponentTransport;

/// Settings sent to the component with [`Request::Init`].
#[derive(Debug, Clone)]
pub struct InitParams {
    /// Host (poly) version.
    pub host_version: String,
    /// Worker threads the component may use.
    pub threads: u32,
    /// Stack size in bytes of the host's worker threads.
    pub stack_size: u64,
    /// Most verbose level the host wants in responses.
    pub max_log_level: WireLogLevel,
}

impl Default for InitParams {
    fn default() -> Self {
        Self {
            host_version: env!("CARGO_PKG_VERSION").to_owned(),
            threads: std::thread::available_parallelism().map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX)),
            stack_size: 16 * 1024 * 1024,
            max_log_level: WireLogLevel::Debug,
        }
    }
}

/// Where a [`ComponentSlot`] stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotStatus {
    /// Not loaded yet; nothing has needed the component.
    Unloaded,
    /// Loaded, initialised and verified against its index entry.
    Ready,
    /// Loading failed; the message is what every later use reports.
    Failed(String),
}

enum State {
    Unloaded,
    Ready(Arc<Connection>),
    Failed(Arc<str>),
}

type Loader = Box<dyn Fn() -> anyhow::Result<Arc<dyn ComponentTransport>> + Send + Sync>;

/// A component that is loaded on first use. Cloned engines of one component share one slot.
pub struct ComponentSlot {
    component_id: String,
    expected: Option<Describe>,
    init: InitParams,
    loader: Loader,
    state: Mutex<State>,
}

impl std::fmt::Debug for ComponentSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComponentSlot")
            .field("component_id", &self.component_id)
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl ComponentSlot {
    /// A slot that runs `loader` on first use. When `expected` is set, the component's
    /// `describe()` must agree with it on the protocol and the engines (the index the host answered
    /// static questions from); the component version is not compared.
    pub fn lazy(
        component_id: impl Into<String>,
        expected: Option<Describe>,
        init: InitParams,
        loader: impl Fn() -> anyhow::Result<Arc<dyn ComponentTransport>> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            component_id: component_id.into(),
            expected,
            init,
            loader: Box::new(loader),
            state: Mutex::new(State::Unloaded),
        })
    }

    /// A slot over an already available transport; it is still initialised lazily.
    pub fn from_transport(
        component_id: impl Into<String>,
        expected: Option<Describe>,
        init: InitParams,
        transport: Arc<dyn ComponentTransport>,
    ) -> Arc<Self> {
        Self::lazy(component_id, expected, init, move || Ok(Arc::clone(&transport)))
    }

    /// A slot that has already failed with `reason`; every use reports it.
    pub fn failed(component_id: impl Into<String>, reason: impl Into<String>) -> Arc<Self> {
        let reason: Arc<str> = Arc::from(reason.into());
        Arc::new(Self {
            component_id: component_id.into(),
            expected: None,
            init: InitParams::default(),
            loader: Box::new(|| anyhow::bail!("component is unavailable")),
            state: Mutex::new(State::Failed(reason)),
        })
    }

    /// The component's id.
    #[must_use]
    pub fn component_id(&self) -> &str {
        &self.component_id
    }

    /// Where the slot stands, without loading anything.
    #[must_use]
    pub fn status(&self) -> SlotStatus {
        match &*self.state.lock().unwrap_or_else(PoisonError::into_inner) {
            State::Unloaded => SlotStatus::Unloaded,
            State::Ready(_) => SlotStatus::Ready,
            State::Failed(reason) => SlotStatus::Failed(reason.to_string()),
        }
    }

    /// The live connection, loading the component on the first call.
    ///
    /// Concurrent first calls serialise on the load, so it happens once. A failed load is
    /// remembered: later calls return the same message without retrying.
    ///
    /// # Errors
    ///
    /// When the component cannot be loaded, initialised or verified against its index entry.
    pub fn connection(&self) -> anyhow::Result<Arc<Connection>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            State::Ready(connection) => return Ok(Arc::clone(connection)),
            State::Failed(reason) => anyhow::bail!("{reason}"),
            State::Unloaded => {}
        }
        // A panic while loading is a load failure too: remembered, never retried with a second Init.
        let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.load())).unwrap_or_else(|payload| {
            Err(anyhow::anyhow!(
                "loading component `{}` panicked: {}",
                self.component_id,
                poly_engine_api::engine_guard::panic_message(payload.as_ref())
            ))
        });
        match loaded {
            Ok(connection) => {
                let connection = Arc::new(connection);
                *state = State::Ready(Arc::clone(&connection));
                Ok(connection)
            }
            Err(error) => {
                let reason: Arc<str> = Arc::from(format!("{error:#}"));
                *state = State::Failed(Arc::clone(&reason));
                anyhow::bail!("{reason}")
            }
        }
    }

    fn load(&self) -> anyhow::Result<Connection> {
        let id = &self.component_id;
        let transport = (self.loader)().with_context(|| format!("failed to load component `{id}`"))?;
        let connection = Connection::new(transport);
        let answer = connection
            .send_plain(&Request::Init {
                protocol: PROTOCOL_VERSION,
                host_version: self.init.host_version.clone(),
                threads: self.init.threads,
                stack_size: self.init.stack_size,
                max_log_level: self.init.max_log_level,
            })
            .with_context(|| format!("component `{id}` rejected Init"))?;
        let Payload::Init { protocol, .. } = answer else {
            anyhow::bail!("component `{id}` answered Init with an unexpected payload");
        };
        anyhow::ensure!(
            protocol == PROTOCOL_VERSION,
            "component `{id}` speaks protocol {protocol}, this poly speaks {PROTOCOL_VERSION}"
        );
        let describe = decode_describe(
            &connection
                .transport
                .describe()
                .with_context(|| format!("describe `{id}`"))?,
        )
        .with_context(|| format!("component `{id}` returned a malformed description"))?;
        if let Some(expected) = &self.expected {
            // The index records no component version, so only the protocol and the engines are
            // compared.
            anyhow::ensure!(
                describe.protocol == expected.protocol && describe.engines == expected.engines,
                "component `{id}` describes itself differently from the index it was resolved with"
            );
        }
        Ok(connection)
    }
}

/// An initialised connection: the transport plus the configs registered on it.
pub struct Connection {
    transport: Arc<dyn ComponentTransport>,
    configs: Mutex<Configs>,
}

struct Configs {
    next: ConfigId,
    ids: HashMap<[u8; 32], ConfigId>,
}

enum CallFailure {
    Wire(WireError),
    Transport(anyhow::Error),
}

impl Connection {
    fn new(transport: Arc<dyn ComponentTransport>) -> Self {
        Self {
            transport,
            configs: Mutex::new(Configs {
                next: 1,
                ids: HashMap::new(),
            }),
        }
    }

    fn send(&self, request: &Request, body: &[u8]) -> Result<Payload, CallFailure> {
        let header = encode_request(request).map_err(|e| CallFailure::Transport(e.into()))?;
        let bytes = self.transport.call(&header, body).map_err(|e| {
            CallFailure::Transport(e.context(format!("component call {} failed", request.variant_name())))
        })?;
        let response: Response = decode_response(&bytes).map_err(|e| {
            CallFailure::Transport(anyhow::Error::new(e).context(format!(
                "component answered {} with a malformed response",
                request.variant_name()
            )))
        })?;
        emit_logs(&response.logs);
        response.result.map_err(CallFailure::Wire)
    }

    fn send_plain(&self, request: &Request) -> anyhow::Result<Payload> {
        self.send(request, &[]).map_err(|failure| match failure {
            CallFailure::Wire(error) => wire_error_to_anyhow("component", error),
            CallFailure::Transport(error) => error,
        })
    }

    fn config_id(&self, fingerprint: [u8; 32], cfg: &EngineConfig) -> anyhow::Result<ConfigId> {
        let mut configs = self.configs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(id) = configs.ids.get(&fingerprint) {
            return Ok(*id);
        }
        let id = configs.next;
        let request = Request::RegisterConfig {
            id,
            config: engine_config_to_wire(cfg),
        };
        match self.send(&request, &[]) {
            Ok(Payload::Ok) => {}
            Ok(_) => anyhow::bail!("component answered RegisterConfig with an unexpected payload"),
            Err(CallFailure::Wire(error)) => return Err(wire_error_to_anyhow("component", error)),
            Err(CallFailure::Transport(error)) => return Err(error),
        }
        configs.next += 1;
        configs.ids.insert(fingerprint, id);
        Ok(id)
    }

    fn forget_config(&self, fingerprint: [u8; 32], id: ConfigId) {
        let mut configs = self.configs.lock().unwrap_or_else(PoisonError::into_inner);
        if configs.ids.get(&fingerprint) == Some(&id) {
            configs.ids.remove(&fingerprint);
        }
    }

    /// A request that names a registered config.
    ///
    /// `cfg` is registered on first use (once per [`EngineConfig::fingerprint`]) and `build`
    /// receives its id. If the component reports the id as unknown, the config is registered
    /// again and the request retried once.
    ///
    /// # Errors
    ///
    /// The component's error with `engine`'s panic wording, or the transport failure.
    pub fn call(
        &self,
        engine: &str,
        cfg: &EngineConfig,
        build: impl Fn(ConfigId) -> Request,
        body: &[u8],
    ) -> anyhow::Result<Payload> {
        let fingerprint = cfg.fingerprint();
        let mut retried = false;
        loop {
            let id = self.config_id(fingerprint, cfg)?;
            match self.send(&build(id), body) {
                Ok(payload) => return Ok(payload),
                Err(CallFailure::Wire(error)) if error.kind == WireErrorKind::UnknownConfig && !retried => {
                    retried = true;
                    self.forget_config(fingerprint, id);
                }
                Err(CallFailure::Wire(error)) => return Err(wire_error_to_anyhow(engine, error)),
                Err(CallFailure::Transport(error)) => return Err(error),
            }
        }
    }

    /// A request that names no config (`SkipReason`, `LintCacheSalt`).
    ///
    /// # Errors
    ///
    /// The component's error with `engine`'s panic wording, or the transport failure.
    pub fn call_plain(&self, engine: &str, request: &Request, body: &[u8]) -> anyhow::Result<Payload> {
        self.send(request, body).map_err(|failure| match failure {
            CallFailure::Wire(error) => wire_error_to_anyhow(engine, error),
            CallFailure::Transport(error) => error,
        })
    }
}

/// Map a component's error to an `anyhow` error.
///
/// A panic reads exactly like the in-process engine guard's error; any other error keeps its
/// message on top and its causes as the context chain.
#[must_use]
pub fn wire_error_to_anyhow(engine: &str, error: WireError) -> anyhow::Error {
    match error.kind {
        // The producer already words a panic like the engine guard (naming the engine that panicked).
        WireErrorKind::Panic => anyhow::anyhow!("{}", error.message),
        WireErrorKind::Error => {
            let mut causes = error.chain.iter().rev();
            let Some(root) = causes.next() else {
                return anyhow::anyhow!("{}", error.message);
            };
            let mut out = anyhow::anyhow!("{root}");
            for cause in causes {
                out = out.context(cause.clone());
            }
            out.context(error.message)
        }
        WireErrorKind::UnknownConfig => anyhow::anyhow!("component lost a registered config: {}", error.message),
        WireErrorKind::UnknownEngine => anyhow::anyhow!("component does not host engine '{engine}': {}", error.message),
        WireErrorKind::Protocol => anyhow::anyhow!("component protocol error: {}", error.message),
        WireErrorKind::Unsupported => anyhow::anyhow!("component does not support the request: {}", error.message),
    }
}

fn emit_logs(logs: &[WireLog]) {
    for log in logs {
        let from = log.target.as_str();
        let message = log.message.as_str();
        match log.level {
            WireLogLevel::Error => tracing::error!(component_target = from, "{message}"),
            WireLogLevel::Warn => tracing::warn!(component_target = from, "{message}"),
            WireLogLevel::Info => tracing::info!(component_target = from, "{message}"),
            WireLogLevel::Debug => tracing::debug!(component_target = from, "{message}"),
            WireLogLevel::Trace => tracing::trace!(component_target = from, "{message}"),
        }
    }
}
