//! Wire DTOs. Engine types are never serialized directly; these mirror them with every field present.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::WireValue;

/// Identifier of a config registered once per component with [`Request::RegisterConfig`].
pub type ConfigId = u32;

/// Severity of a [`WireDiagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum WireSeverity {
    /// A problem that should fail the run.
    Error,
    /// A likely problem.
    Warning,
    /// Informational note.
    Info,
    /// A low-priority suggestion.
    Hint,
}

/// 1-based line/column span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireSpan {
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based start column.
    pub start_col: u32,
    /// 1-based end line.
    pub end_line: u32,
    /// 1-based end column.
    pub end_col: u32,
}

/// A byte-range replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireEdit {
    /// Inclusive start byte offset.
    pub start_byte: u64,
    /// Exclusive end byte offset.
    pub end_byte: u64,
    /// Replacement text.
    pub replacement: String,
}

/// A normalized lint finding. Every field is always present on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireDiagnostic {
    /// Id of the engine that produced the finding.
    pub engine: String,
    /// Tool-specific rule code.
    pub code: Option<String>,
    /// Severity.
    pub severity: WireSeverity,
    /// One-line title.
    pub title: String,
    /// Longer explanation.
    pub description: Option<String>,
    /// Source location.
    pub span: Option<WireSpan>,
    /// Rule documentation URL.
    pub url: Option<String>,
    /// Suggested autofix, applied atomically; empty when there is none.
    pub fix: Vec<WireEdit>,
    /// Tool-specific extras.
    pub metadata: BTreeMap<String, String>,
}

/// A file whose content is the entire `body` of the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireFile {
    /// Path, used for naming and config discovery only.
    pub path: String,
    /// Language id (see `Language::id`).
    pub language: String,
    /// Byte length of the content in `body`; must equal `body.len()`.
    pub len: u64,
}

/// One member of a batch: a byte range of the call's `body`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireFileSlice {
    /// Path, used for naming and config discovery only.
    pub path: String,
    /// Language id.
    pub language: String,
    /// Offset of the content within `body`.
    pub offset: u64,
    /// Byte length of the content.
    pub len: u64,
}

/// Which config table an option declaration describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum WireOptionTable {
    /// `[lint.<lang>.<engine>]`.
    Lint,
    /// `[fmt.<lang>.<engine>]`.
    Format,
    /// The language-agnostic `[lint.<engine>]` table of a cross-cutting engine.
    CrossCuttingLint,
}

/// Whether a query concerns linting or formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum WireKind {
    /// Lint.
    Lint,
    /// Format.
    Format,
}

/// One declared option key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireOptionKey {
    /// Key name.
    pub name: String,
    /// Accepted value types as bits: 1 boolean, 2 integer, 4 float, 8 string, 16 array, 32 table, 64 datetime.
    pub types: u8,
}

/// The static option keys an engine reads from one table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireOptionKeys {
    /// The table described.
    pub table: WireOptionTable,
    /// `false` when the engine declares nothing and no key is reported.
    pub checked: bool,
    /// Keys read by name.
    pub declared: Vec<WireOptionKey>,
    /// The engine accepts poly's uniform rule-selection vocabulary.
    pub rule_selection: bool,
    /// Further keys are answered at run time by [`Request::DerivedOptionKeys`].
    pub has_derived: bool,
    /// Guidance shown with unknown-key warnings.
    pub note: Option<String>,
}

/// What an engine can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireCapabilities {
    /// Can report diagnostics.
    pub lint: bool,
    /// Can reformat source.
    pub format: bool,
    /// Can produce autofixes.
    pub fix: bool,
}

/// Which batch operations an engine supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireBatchSupport {
    /// `format_batch` is implemented.
    pub format: bool,
    /// `lint_batch` is implemented.
    pub lint: bool,
}

/// Bits of [`WireEngineDesc::hooks`]: which optional per-engine requests the host must send.
/// An unset bit means the host answers with the trait default and never makes the call.
pub mod hooks {
    /// [`Request::RunContext`](crate::Request::RunContext).
    pub const RUN_CONTEXT: u32 = 1 << 0;
    /// [`Request::FileContext`](crate::Request::FileContext).
    pub const FILE_CONTEXT: u32 = 1 << 1;
    /// [`Request::ProvidesLanguage`](crate::Request::ProvidesLanguage).
    pub const PROVIDES_LANGUAGE: u32 = 1 << 2;
    /// [`Request::SkipReason`](crate::Request::SkipReason).
    pub const SKIP_REASON: u32 = 1 << 3;
    /// [`Request::LintInputSalt`](crate::Request::LintInputSalt).
    pub const LINT_INPUT_SALT: u32 = 1 << 4;
    /// [`Request::FormatInputSalt`](crate::Request::FormatInputSalt).
    pub const FORMAT_INPUT_SALT: u32 = 1 << 5;
    /// [`Request::LintCacheSalt`](crate::Request::LintCacheSalt).
    pub const LINT_CACHE_SALT: u32 = 1 << 6;
    /// [`Request::BatchSupport`](crate::Request::BatchSupport).
    pub const BATCH_SUPPORT: u32 = 1 << 7;
}

/// Static description of one engine hosted by a component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireEngineDesc {
    /// Tool id (`Engine::name`).
    pub name: String,
    /// Language ids served; empty means cross-cutting.
    pub languages: Vec<String>,
    /// Capabilities.
    pub capabilities: WireCapabilities,
    /// Tool version, folded into the cache key.
    pub version: String,
    /// Static option keys per table.
    pub option_keys: Vec<WireOptionKeys>,
    /// Bit set of [`hooks`] the host must call.
    pub hooks: u32,
    /// `Engine::takes_over_tier_one`.
    pub takes_over_tier_one: bool,
    /// `Engine::claimed_paths`.
    pub claimed_paths: Vec<String>,
    /// `Engine::supplements_tier_one`.
    pub supplements_tier_one: bool,
    /// `Engine::self_manages_enabled`.
    pub self_manages_enabled: bool,
    /// `Engine::supersedes_generic_formatter`.
    pub supersedes_generic_formatter: bool,
    /// `Engine::batch_shard_floor`.
    pub batch_shard_floor: u32,
}

/// The answer to `describe()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Describe {
    /// Protocol version the component speaks.
    pub protocol: u32,
    /// Version of the component itself.
    pub component_version: String,
    /// Hosted engines.
    pub engines: Vec<WireEngineDesc>,
}

/// Log level, ordered from most to least severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum WireLogLevel {
    /// Errors.
    Error,
    /// Warnings.
    Warn,
    /// Informational.
    Info,
    /// Debug.
    Debug,
    /// Trace.
    Trace,
}

/// A log record emitted by the component during a call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireLog {
    /// Level.
    pub level: WireLogLevel,
    /// Emitting target (module path).
    pub target: String,
    /// Message.
    pub message: String,
}

/// Classification of a [`WireError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum WireErrorKind {
    /// The engine returned an error.
    Error,
    /// The engine panicked; the call was caught.
    Panic,
    /// A request named a [`ConfigId`] that was never registered.
    UnknownConfig,
    /// A request named an engine the component does not host.
    UnknownEngine,
    /// The request was malformed or violated the protocol.
    Protocol,
}

/// A failed call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireError {
    /// Classification.
    pub kind: WireErrorKind,
    /// Top-level message.
    pub message: String,
    /// Causes, outermost first.
    pub chain: Vec<String>,
}

/// A request header. File content never appears here; it travels in the call's `body`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum Request {
    /// First call: negotiate the protocol and pass host settings.
    Init {
        /// Protocol version the host speaks.
        protocol: u32,
        /// Host (poly) version.
        host_version: String,
        /// Worker threads the component may use.
        threads: u32,
        /// Stack size in bytes of the host's worker threads.
        stack_size: u64,
        /// Most verbose level the host wants in [`Response::logs`].
        max_log_level: WireLogLevel,
    },
    /// Register a config; later requests refer to it by `id`.
    RegisterConfig {
        /// Id chosen by the host, unique per component.
        id: ConfigId,
        /// The engine config: a table with `indent_width` and `options`, plus globals.
        config: WireValue,
    },
    /// Lint one file (content in `body`).
    Lint {
        /// Engine name.
        engine: String,
        /// Registered config.
        config: ConfigId,
        /// The file.
        file: WireFile,
    },
    /// Format one file (content in `body`).
    Format {
        /// Engine name.
        engine: String,
        /// Registered config.
        config: ConfigId,
        /// The file.
        file: WireFile,
    },
    /// Lint a batch; each member is a slice of `body`.
    LintBatch {
        /// Engine name.
        engine: String,
        /// Registered config.
        config: ConfigId,
        /// Members, in order.
        files: Vec<WireFileSlice>,
    },
    /// Format a batch; each member is a slice of `body`.
    FormatBatch {
        /// Engine name.
        engine: String,
        /// Registered config.
        config: ConfigId,
        /// Members, in order.
        files: Vec<WireFileSlice>,
    },
    /// Whole-project check (no file content; the component reads from `root`).
    ProjectCheck {
        /// Project root.
        root: String,
        /// Files in scope, relative to `root`.
        files: Vec<String>,
        /// Registered config.
        config: ConfigId,
        /// Environment variables the check may see.
        environment: Vec<(String, String)>,
    },
    /// `Engine::skip_reason` for one file (content in `body`).
    SkipReason {
        /// Engine name.
        engine: String,
        /// The file.
        file: WireFile,
    },
    /// `Engine::lint_cache_salt` for one file (content in `body`).
    LintCacheSalt {
        /// Engine name.
        engine: String,
        /// The file.
        file: WireFile,
    },
    /// `Engine::provides_language_lint` / `provides_language_format`.
    ProvidesLanguage {
        /// Engine name.
        engine: String,
        /// Registered config.
        config: ConfigId,
        /// Language id.
        language: String,
        /// Which of the two predicates.
        kind: WireKind,
    },
    /// `Engine::batch_support`.
    BatchSupport {
        /// Engine name.
        engine: String,
        /// Registered config.
        config: ConfigId,
    },
    /// The keys a serde-derived probe recognises in `options` (see [`WireOptionKeys::has_derived`]).
    DerivedOptionKeys {
        /// Engine name.
        engine: String,
        /// Table the keys belong to.
        table: WireOptionTable,
        /// The user's options table.
        options: WireValue,
    },
    /// `Engine::run_context`.
    RunContext {
        /// Engine name.
        engine: String,
        /// Registered config.
        config: ConfigId,
    },
    /// `Engine::file_context`.
    FileContext {
        /// Engine name.
        engine: String,
        /// Registered config.
        config: ConfigId,
        /// File path.
        path: String,
    },
    /// `Engine::lint_input_salt`.
    LintInputSalt {
        /// Engine name.
        engine: String,
        /// Registered config.
        config: ConfigId,
        /// File path.
        path: String,
    },
    /// `Engine::format_input_salt`.
    FormatInputSalt {
        /// Engine name.
        engine: String,
        /// Registered config.
        config: ConfigId,
        /// File path.
        path: String,
    },
}

/// Names of every [`Request`] variant, in declaration order.
pub const REQUEST_VARIANTS: &[&str] = &[
    "Init",
    "RegisterConfig",
    "Lint",
    "Format",
    "LintBatch",
    "FormatBatch",
    "ProjectCheck",
    "SkipReason",
    "LintCacheSalt",
    "ProvidesLanguage",
    "BatchSupport",
    "DerivedOptionKeys",
    "RunContext",
    "FileContext",
    "LintInputSalt",
    "FormatInputSalt",
];

impl Request {
    /// The variant name, as listed in [`REQUEST_VARIANTS`].
    pub fn variant_name(&self) -> &'static str {
        match self {
            Request::Init { .. } => "Init",
            Request::RegisterConfig { .. } => "RegisterConfig",
            Request::Lint { .. } => "Lint",
            Request::Format { .. } => "Format",
            Request::LintBatch { .. } => "LintBatch",
            Request::FormatBatch { .. } => "FormatBatch",
            Request::ProjectCheck { .. } => "ProjectCheck",
            Request::SkipReason { .. } => "SkipReason",
            Request::LintCacheSalt { .. } => "LintCacheSalt",
            Request::ProvidesLanguage { .. } => "ProvidesLanguage",
            Request::BatchSupport { .. } => "BatchSupport",
            Request::DerivedOptionKeys { .. } => "DerivedOptionKeys",
            Request::RunContext { .. } => "RunContext",
            Request::FileContext { .. } => "FileContext",
            Request::LintInputSalt { .. } => "LintInputSalt",
            Request::FormatInputSalt { .. } => "FormatInputSalt",
        }
    }
}

/// Result of formatting one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum WireFormatOutput {
    /// Already formatted.
    Unchanged,
    /// The formatted source.
    Formatted(String),
}

/// Per-member result of a batch lint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum WireLintItem {
    /// Diagnostics for the member.
    Ok(Vec<WireDiagnostic>),
    /// The member failed on its own.
    Err(WireError),
}

/// Per-member result of a batch format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum WireFormatItem {
    /// Output for the member.
    Ok(WireFormatOutput),
    /// The member failed on its own.
    Err(WireError),
}

/// Diagnostics of one file from a project check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WireFileDiagnostics {
    /// Path as given in the request.
    pub path: String,
    /// Findings for that file.
    pub diagnostics: Vec<WireDiagnostic>,
}

/// The successful payload of a [`Response`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum Payload {
    /// Success with no data (`RegisterConfig`).
    Ok,
    /// Answer to `Init`.
    Init {
        /// Protocol version the component speaks.
        protocol: u32,
        /// Component version.
        component_version: String,
    },
    /// Answer to `Lint`.
    Diagnostics(Vec<WireDiagnostic>),
    /// Answer to `Format`.
    Formatted(WireFormatOutput),
    /// Answer to `LintBatch`: one item per member, in order.
    LintBatch(Vec<WireLintItem>),
    /// Answer to `FormatBatch`: one item per member, in order.
    FormatBatch(Vec<WireFormatItem>),
    /// Answer to `ProjectCheck`.
    Project(Vec<WireFileDiagnostics>),
    /// Answer to `ProvidesLanguage`.
    Bool(bool),
    /// Answer to `SkipReason`, the salts and the contexts; `None` is the trait default.
    OptString(Option<String>),
    /// Answer to `RunContext`.
    Text(String),
    /// Answer to `BatchSupport`.
    BatchSupport(WireBatchSupport),
    /// Answer to `DerivedOptionKeys`; `None` means the probe could not tell.
    OptionKeys(Option<Vec<String>>),
}

/// The answer to every call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Response {
    /// Outcome.
    pub result: Result<Payload, WireError>,
    /// Log records produced during the call.
    pub logs: Vec<WireLog>,
}

/// Root type used to emit one JSON Schema covering every top-level message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProtocolSchema {
    /// Request header.
    pub request: Request,
    /// Response.
    pub response: Response,
    /// `describe()` payload.
    pub describe: Describe,
}
