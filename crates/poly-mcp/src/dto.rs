//! Data-transfer objects and result builders for the MCP tools.
//!
//! Every tool returns rmcp **structured content**: the typed payload lands in
//! [`rmcp::model::CallToolResult::structured_content`] (its JSON schema is
//! attached to the tool definition via `#[tool(output_schema = …)]`), and a
//! single text [`ContentBlock`] carries either the same JSON or a compact
//! **TOON** rendering, selected per request by [`TextRepr`]. A client thus gets
//! machine-readable structured JSON *and* a human/compact text view from one
//! call.
//!
//! Lint and format return `poly-core`'s own `LintDocument` / `FormatDocument`
//! — the exact value `poly lint --format json` prints. The host renders it
//! (see [`RunReply`]) and publishes its JSON Schema, so this crate links no
//! engine and there is no MCP-local copy: the two surfaces cannot answer the
//! same question differently.
//!
//! The document is built from the whole *run*, not just its results, and states
//! three things a caller would otherwise have to reconstruct by walking every
//! record: files the run **failed** on (also flipping `isError`, the MCP
//! stand-in for the CLI's exit code 2), files nothing **inspected**, and how
//! many files were actually **checked**. A run where everything was skipped
//! reports no diagnostics and is not an error — without those fields it is
//! indistinguishable from a clean pass, which is the defect they close. A skip
//! is coverage information, not a failure, so it deliberately does not set
//! `isError`.
//!
//! A lint document additionally carries `suppressed` — one entry per diagnostic
//! the run found and then dropped, naming the file, the rule, and which
//! mechanism dropped it (`default-path-exclusion`, `per-file-ignore`,
//! `inline-suppression`). None of those is a coverage limitation, so like a
//! skip none sets `isError`; but `results` plus `suppressed` is the unfiltered
//! finding set, so a caller can see what a `poly.toml` or an in-source
//! directive removed without re-running with the filters off. It is omitted
//! when empty, unlike `errors`/`skipped`, which are always present because a
//! consumer must be able to test them unconditionally.
//!
//! The cache/rules/config/workspace DTOs live in [`poly_frontend_api::dto`],
//! shared with the host that builds them, because their CLI counterparts print
//! prose rather than a serializable value.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

pub use poly_frontend_api::dto::{
    CacheCleanReport, CacheNamespace, CacheStatsReport, ComponentAction, ComponentActions, ComponentRow,
    ComponentSummary, ComponentsStatus, ConfigDefaults, ConfigShowReport, RuleInfo, RuleTestOutcome, RulesReport,
    WorkspaceReport, WorkspaceToolReport,
};
use poly_frontend_api::wire::{RunReply, Schemas};
use rmcp::ErrorData;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, MetaObject};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::identity::{IDENTITY_KEY, PolyIdentity, identity_value};

/// Which text representation a tool should place in its text content block.
/// `structured_content` is always JSON regardless; this selects only the text
/// block so a client can ask for the compact TOON view.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TextRepr {
    /// Pretty JSON (the default) — mirrors the CLI `--format json` text.
    #[default]
    Json,
    /// Token-Oriented Object Notation — mirrors the CLI `--format toon` text.
    Toon,
}

/// Map a serialization or render failure onto an MCP internal error.
///
/// Always an error, never a substitute payload: a caller that receives a
/// well-formed but empty document cannot tell it from a clean run.
fn serialize_error(error: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(format!("failed to serialize tool output: {error}"), None)
}

/// Assemble a [`CallToolResult`] carrying structured JSON plus a chosen text
/// block. `structured` is always the JSON value; the text block is `json_text`
/// or `toon_text` per `repr`.
///
/// Every result also carries the serving binary's [`PolyIdentity`], both as a
/// `poly` key in `structured_content` and in the response `_meta`. An MCP caller
/// has no `poly --version` to fall back on, so a result that does not say which
/// binary produced it is indistinguishable from one produced by a build with
/// known-destructive behaviour.
fn build(structured: Value, json_text: String, toon_text: String, repr: TextRepr) -> CallToolResult {
    build_with_status(structured, json_text, toon_text, repr, false)
}

/// [`build`], additionally marking the result as a tool-level **error**.
///
/// A run that could not process a file did not check it, and a caller that reads
/// only the tool's success/failure flag must not be told otherwise — the MCP
/// counterpart of the CLI's exit code 2. The payload is unchanged: the caller
/// still gets every result the run did produce, alongside the failures.
fn build_with_status(
    structured: Value,
    json_text: String,
    toon_text: String,
    repr: TextRepr,
    is_error: bool,
) -> CallToolResult {
    let text = match repr {
        TextRepr::Json => json_text,
        TextRepr::Toon => toon_text,
    };
    // `CallToolResult::structured` seeds `structured_content` and a JSON text
    // block; override the text block with the representation the caller asked
    // for while leaving `structured_content` as JSON. ~keep
    let mut result = CallToolResult::structured(with_identity(structured));
    result.content = vec![ContentBlock::text(text)];
    result.meta = Some(identity_meta());
    result.is_error = Some(is_error);
    result
}

/// Add the serving binary's identity under the `poly` key. A non-object payload
/// is left alone — `_meta` still carries the identity in that case.
fn with_identity(structured: Value) -> Value {
    match structured {
        Value::Object(mut map) => {
            map.insert(IDENTITY_KEY.to_string(), identity_value().clone());
            Value::Object(map)
        }
        other => other,
    }
}

/// The response `_meta` block carrying the serving binary's identity.
fn identity_meta() -> MetaObject {
    let mut meta = MetaObject::new();
    meta.0.insert(IDENTITY_KEY.to_string(), identity_value().clone());
    meta
}

/// The output schema for `T`, extended with the `poly` identity property that
/// [`build`] injects, so the declared schema matches what is actually sent.
pub fn schema_for_output_with_identity<T: JsonSchema + std::any::Any>() -> Arc<JsonObject> {
    with_identity_property(rmcp::handler::server::tool::schema_for_output::<T>())
}

/// The root schemas the host published for the lint and format documents.
static DOCUMENT_SCHEMAS: OnceLock<Schemas> = OnceLock::new();

/// Remember the host's document schemas so the lint and format tools can declare them. The first
/// call wins: the host's documents do not change while it runs.
pub fn install_document_schemas(schemas: Schemas) {
    let _ = DOCUMENT_SCHEMAS.set(schemas);
}

/// The declared output schema of the lint tools, with the identity property.
///
/// Before the host's schema is known it is the bare object schema carrying only the identity, so
/// a tool list never fails for want of it.
pub fn lint_output_schema() -> Arc<JsonObject> {
    document_output_schema(DOCUMENT_SCHEMAS.get().map(|schemas| &schemas.lint))
}

/// The declared output schema of the format tools; see [`lint_output_schema`].
pub fn format_output_schema() -> Arc<JsonObject> {
    document_output_schema(DOCUMENT_SCHEMAS.get().map(|schemas| &schemas.format))
}

/// A host-published root schema in the shape rmcp gives an output schema (top-level `title` and
/// `description` removed), plus the identity property.
fn document_output_schema(published: Option<&Value>) -> Arc<JsonObject> {
    let mut object = match published {
        Some(Value::Object(object)) => object.clone(),
        _ => {
            let mut object = JsonObject::new();
            object.insert("type".to_string(), Value::String("object".to_string()));
            object.insert("properties".to_string(), Value::Object(JsonObject::new()));
            object
        }
    };
    object.remove("title");
    object.remove("description");
    with_identity_property(Arc::new(object))
}

/// Extend an object schema with the `poly` identity property.
fn with_identity_property(base: Arc<JsonObject>) -> Arc<JsonObject> {
    let mut object = base.as_ref().clone();
    // Only object schemas have properties to extend; anything else is returned
    // unchanged rather than reshaped.
    if !matches!(object.get("properties"), Some(Value::Object(_))) {
        return base;
    }
    if let Some(Value::Object(properties)) = object.get_mut("properties") {
        properties.insert(IDENTITY_KEY.to_string(), identity_schema());
    }
    match object.get_mut("required") {
        Some(Value::Array(required)) => required.push(Value::String(IDENTITY_KEY.to_string())),
        _ => {
            object.insert(
                "required".to_string(),
                Value::Array(vec![Value::String(IDENTITY_KEY.to_string())]),
            );
        }
    }
    Arc::new(object)
}

/// The inline JSON schema for [`PolyIdentity`], with the document-level keys
/// that do not belong on a nested subschema removed.
fn identity_schema() -> Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(PolyIdentity)).unwrap_or(Value::Bool(true));
    if let Value::Object(map) = &mut schema {
        map.remove("$schema");
        map.remove("title");
    }
    schema
}

/// Build a tool result for a self-describing DTO: structured JSON plus a JSON or
/// TOON text block derived from the same value.
pub fn dto_result<T: Serialize>(value: &T, repr: TextRepr) -> Result<CallToolResult, ErrorData> {
    let structured = serde_json::to_value(value).map_err(serialize_error)?;
    let json_text = serde_json::to_string_pretty(value).map_err(serialize_error)?;
    let toon_text = serde_toon::to_string(value).unwrap_or_else(|_| json_text.clone());
    Ok(build(structured, json_text, toon_text, repr))
}

/// The result of an install or uninstall. A component that failed or was refused is a tool error:
/// the caller asked for it to be usable (or gone) and it is not.
pub fn actions_result(actions: &ComponentActions, repr: TextRepr) -> Result<CallToolResult, ErrorData> {
    let structured = serde_json::to_value(actions).map_err(serialize_error)?;
    let json_text = serde_json::to_string_pretty(actions).map_err(serialize_error)?;
    let toon_text = serde_toon::to_string(actions).unwrap_or_else(|_| json_text.clone());
    Ok(build_with_status(structured, json_text, toon_text, repr, !actions.ok))
}

/// Build the tool result for a whole lint run.
///
/// The payload is the host's rendering of `poly-core`'s own `LintDocument` — the same value
/// `poly lint --format json` prints — so `structured_content` and the text block
/// carry one shape, and the CLI and this server cannot answer "what did you
/// actually check" differently. A render failure never reaches here: the host
/// reports it as a failed call, which becomes a tool error rather than an empty
/// document, since an MCP caller has no other way to tell "clean" from "poly
/// could not tell you".
pub fn lint_result(reply: RunReply, repr: TextRepr) -> CallToolResult {
    run_result(reply, repr)
}

/// Build the tool result for a whole format run — the counterpart of
/// [`lint_result`], and for the same reasons.
pub fn format_result(reply: RunReply, repr: TextRepr) -> CallToolResult {
    run_result(reply, repr)
}

fn run_result(reply: RunReply, repr: TextRepr) -> CallToolResult {
    build_with_status(reply.document, reply.json, reply.toon, repr, reply.is_error)
}

/// Health of the binary serving this MCP session (the `version` tool).
///
/// The identity itself rides in the `poly` key every response carries; this adds
/// what only a long-lived server can tell you — whether the executable it is
/// running from is still the one installed on disk, and how long it has been
/// answering.
#[derive(Debug, Serialize, JsonSchema)]
pub struct VersionReport {
    /// Whether the executable this server started from is still the file at
    /// that path. `false` means an upgrade (or a delete) happened underneath a
    /// running server, and every other tool is refusing to answer.
    pub executable_current: bool,
    /// Human-readable explanation of the executable's state.
    pub executable_status: String,
    /// Seconds this server process has been running. A server older than your
    /// last upgrade is serving the pre-upgrade build.
    pub uptime_seconds: u64,
    /// Every compiled-in backend and the upstream version it wraps.
    ///
    /// The identity block on every response carries a *digest* of this, which is
    /// enough to tell two binaries apart but not to say how. This is the
    /// expansion, returned only here so the map is not repeated on every result.
    /// Host-toolchain backends are excluded: their version depends on the
    /// machine and the checkout, not on the binary.
    pub engines: BTreeMap<String, String>,
    /// The downloadable components and where each stands (installed, not installed,
    /// unavailable). Empty when the host has no component state to report.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ComponentSummary>,
}
