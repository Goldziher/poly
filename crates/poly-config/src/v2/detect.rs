//! Detection of v1-only constructs in a parsed `poly.toml`.
//!
//! A v1 config is a hard load error in v2. The detection is a pure function over
//! the raw table so the loader (wired in #63, together with the `poly migrate
//! --write` importer from #64) can call it before typed deserialization. It is
//! deliberately not called from the current loader: poly's own `poly.toml` is
//! still v1.

use std::fmt;

/// A construct that exists only in the v1 schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum V1Marker {
    /// A top-level `[lint]` table (per-language engine tables; v2 uses
    /// `[engines.<id>]`).
    LintTable,
    /// A top-level `[fmt]` table (see [`V1Marker::LintTable`]).
    FmtTable,
    /// `[lint] workspace = ...` (v2: `[workspace] run`).
    LintWorkspace,
    /// A top-level `[rules]` table (v2: `[engines.astgrep] rule_dirs` /
    /// `builtin_pack`).
    RulesTable,
    /// `[workspace] root = ...` (v2: top-level `root`).
    WorkspaceRoot,
    /// The hyphenated `per-file-ignores` table (v2: `per_file_ignores`).
    PerFileIgnores,
}

impl V1Marker {
    /// The v1 spelling, as a user would find it in their file.
    #[must_use]
    pub fn v1_key(self) -> &'static str {
        match self {
            V1Marker::LintTable => "[lint]",
            V1Marker::FmtTable => "[fmt]",
            V1Marker::LintWorkspace => "[lint] workspace",
            V1Marker::RulesTable => "[rules]",
            V1Marker::WorkspaceRoot => "[workspace] root",
            V1Marker::PerFileIgnores => "[per-file-ignores]",
        }
    }

    /// What replaces it in v2.
    #[must_use]
    pub fn v2_replacement(self) -> &'static str {
        match self {
            V1Marker::LintTable | V1Marker::FmtTable => "[engines.<id>] (with .lint / .format / .lang.<language>)",
            V1Marker::LintWorkspace => "[workspace] run = \"auto\" | \"always\" | \"never\"",
            V1Marker::RulesTable => "[engines.astgrep] rule_dirs / builtin_pack",
            V1Marker::WorkspaceRoot => "top-level `root = true`",
            V1Marker::PerFileIgnores => "[per_file_ignores]",
        }
    }
}

impl fmt::Display for V1Marker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (use {})", self.v1_key(), self.v2_replacement())
    }
}

/// Every v1-only construct present in `table`, in a stable order. Empty for a
/// v2 config.
#[must_use]
pub fn detect_v1(table: &toml::Table) -> Vec<V1Marker> {
    let mut found = Vec::new();
    if let Some(lint) = table.get("lint") {
        found.push(V1Marker::LintTable);
        if lint.as_table().is_some_and(|t| t.contains_key("workspace")) {
            found.push(V1Marker::LintWorkspace);
        }
    }
    if table.contains_key("fmt") {
        found.push(V1Marker::FmtTable);
    }
    if table.contains_key("rules") {
        found.push(V1Marker::RulesTable);
    }
    if table
        .get("workspace")
        .and_then(toml::Value::as_table)
        .is_some_and(|t| t.contains_key("root"))
    {
        found.push(V1Marker::WorkspaceRoot);
    }
    if table.contains_key("per-file-ignores") {
        found.push(V1Marker::PerFileIgnores);
    }
    found.sort();
    found
}

/// The hard-error message for a v1 config, naming each marker and the
/// `poly migrate --write` fix.
#[must_use]
pub fn v1_error(markers: &[V1Marker]) -> String {
    let mut message = String::from("this poly.toml uses the v1 schema, which poly v2 no longer reads:\n");
    for marker in markers {
        message.push_str("  - ");
        message.push_str(&marker.to_string());
        message.push('\n');
    }
    message.push_str("run `poly migrate --write` to convert it to the v2 schema");
    message
}
