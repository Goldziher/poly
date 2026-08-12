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
//! The lint/format DTOs wrap the `poly-core` report types verbatim (their
//! `JsonSchema` derives are gated behind the crate's `schemars` feature, enabled
//! by this crate), so the structured payload is exactly the CLI's `--format
//! json` data. The cache/rules/config/workspace DTOs are MCP-local because their
//! CLI counterparts print prose rather than a serializable value.

use std::sync::Arc;

use poly_core::{FormatResult, LintResult};
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

/// Map an infallible serialization failure onto an MCP internal error.
fn serialize_error(error: serde_json::Error) -> ErrorData {
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
    let base = rmcp::handler::server::tool::schema_for_output::<T>();
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

/// Structured lint results (mirrors `poly lint --format json`). Wraps the
/// `poly-core` per-file results so `structured_content` is a self-describing
/// object with a stable output schema.
#[derive(Debug, Serialize, JsonSchema)]
pub struct LintReport {
    /// Per-file lint outcome, identical to the CLI JSON records.
    pub results: Vec<LintResult>,
}

impl LintReport {
    /// Build the structured result. The JSON/TOON text blocks reproduce the CLI
    /// array exactly (`report_lint_json` / `report_lint_toon`), so the text
    /// contract is unchanged while `structured_content` gains the object schema.
    pub fn into_result(self, repr: TextRepr) -> Result<CallToolResult, ErrorData> {
        let structured = serde_json::to_value(&self).map_err(serialize_error)?;
        let json_text = poly_core::report::report_lint_json(&self.results);
        let toon_text = poly_core::report::report_lint_toon(&self.results);
        Ok(build(structured, json_text, toon_text, repr))
    }
}

/// Structured format results (mirrors `poly fmt --format json`).
#[derive(Debug, Serialize, JsonSchema)]
pub struct FormatReport {
    /// Per-file format outcome, identical to the CLI JSON records.
    pub results: Vec<FormatResult>,
}

impl FormatReport {
    /// Build the structured result, reproducing the CLI JSON/TOON array in the
    /// text block.
    pub fn into_result(self, repr: TextRepr) -> Result<CallToolResult, ErrorData> {
        let structured = serde_json::to_value(&self).map_err(serialize_error)?;
        let json_text = poly_core::report::report_format_json(&self.results);
        let toon_text = poly_core::report::report_format_toon(&self.results);
        Ok(build(structured, json_text, toon_text, repr))
    }
}

/// One result-cache namespace's footprint.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CacheNamespace {
    /// Namespace directory name (e.g. `results`, `hooks`).
    pub namespace: String,
    /// Number of cached entries in this namespace.
    pub entries: u64,
    /// On-disk size of this namespace, in bytes.
    pub bytes: u64,
}

/// Result-cache footprint (mirrors `poly cache stats`).
#[derive(Debug, Serialize, JsonSchema)]
pub struct CacheStatsReport {
    /// Cache format version this binary writes.
    pub format_version: String,
    /// Cache format version found on disk, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_disk_version: Option<String>,
    /// Total on-disk size across all namespaces, in bytes.
    pub total_bytes: u64,
    /// Per-namespace footprint.
    pub per_namespace: Vec<CacheNamespace>,
}

/// Freed-byte report for `poly cache clean`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CacheCleanReport {
    /// Bytes reclaimed by removing every cached entry.
    pub freed_bytes: u64,
}

/// One discovered custom ast-grep rule.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RuleInfo {
    /// Rule id.
    pub id: String,
    /// Target language name.
    pub language: String,
    /// Declared severity.
    pub severity: String,
}

/// One rule-test snippet outcome.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RuleTestOutcome {
    /// The rule id the snippet was checked against.
    pub rule_id: String,
    /// Snippet expectation: `valid`, `invalid`, or `fixed`.
    pub kind: String,
    /// Index of the snippet within its list.
    pub index: usize,
    /// Whether the snippet met its expectation.
    pub passed: bool,
    /// Failure detail, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Custom rule inventory and optional test report (mirrors `poly rules`).
#[derive(Debug, Serialize, JsonSchema)]
pub struct RulesReport {
    /// Rule directories that were searched.
    pub dirs: Vec<String>,
    /// Discovered rules.
    pub rules: Vec<RuleInfo>,
    /// Per-snippet test outcomes, present only when `test` was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests: Option<Vec<RuleTestOutcome>>,
    /// Test files naming an unknown rule id (only when `test` was requested).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_rule_ids: Option<Vec<String>>,
    /// Rules with no test file (only when `test` was requested).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub untested_rule_ids: Option<Vec<String>>,
    /// Whether every snippet passed and every test named a real rule (only when
    /// `test` was requested).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
}

/// Effective `[defaults]` section (mirrors `poly config show`).
#[derive(Debug, Serialize, JsonSchema)]
pub struct ConfigDefaults {
    /// Opinionated line length.
    pub line_length: usize,
    /// Line ending policy (debug rendering of the enum).
    pub line_ending: String,
    /// Whether a trailing final newline is enforced.
    pub final_newline: bool,
    /// Whether trailing whitespace is trimmed.
    pub trim_trailing_whitespace: bool,
}

/// The effective, merged config summary (mirrors `poly config show`, without
/// remote-`extends` fetching — see the network-free `ops::resolve_poly_config`).
#[derive(Debug, Serialize, JsonSchema)]
pub struct ConfigShowReport {
    /// Resolved config file path.
    pub config_path: String,
    /// Effective defaults.
    pub defaults: ConfigDefaults,
    /// Configured `[lint.<lang>.<tool>]` top-level keys.
    pub lint_keys: Vec<String>,
    /// Configured `[fmt.<lang>.<tool>]` top-level keys.
    pub fmt_keys: Vec<String>,
    /// Opted-in catalog tool names.
    pub tools: Vec<String>,
    /// Whether a `[hooks]` section is present.
    pub hooks_present: bool,
    /// Custom ast-grep rule directories.
    pub rule_dirs: Vec<String>,
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
}

/// One whole-project tool's outcome.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkspaceToolReport {
    /// Hook id (e.g. `cargo-clippy`) or inline job label.
    pub id: String,
    /// Whether the tool reported a failure.
    pub failed: bool,
    /// Whether the result was served from cache.
    pub cached: bool,
    /// The tool's captured combined output (lossily decoded UTF-8).
    pub output: String,
}

/// Whole-project (workspace) lint outcome (mirrors `poly lint`'s whole-project
/// phase). Produced by the long-running `workspace_lint` / `workspace_lint_fix`
/// Task tools.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkspaceReport {
    /// Overall pass/fail: `false` means a whole-project tool reported failures.
    pub passed: bool,
    /// Whether autofixes were applied (`workspace_lint_fix`).
    pub fixed: bool,
    /// Per-tool results, in run order. Empty when the phase did not run.
    pub tools: Vec<WorkspaceToolReport>,
}
