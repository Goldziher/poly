//! Catalog data model: one [`Tool`] per wrapped CLI, each with a map of named
//! [`Command`]s. Deserialized from the vendored `data/catalog.json` (see
//! `data/README.md` for provenance).

use std::collections::BTreeMap;

use serde::Deserialize;

/// The sole argument placeholder used by catalog commands. `poly` substitutes it
/// with the concrete file path at invocation time (see [`Command::argv`]).
pub const PATH_PLACEHOLDER: &str = "$PATH";

/// The `formatter` category string used by the catalog.
pub const CATEGORY_FORMATTER: &str = "formatter";
/// The `linter` category string used by the catalog.
pub const CATEGORY_LINTER: &str = "linter";
/// The `spell-check` category string used by the catalog.
pub const CATEGORY_SPELL_CHECK: &str = "spell-check";

/// A single wrapped CLI tool: its binary, the languages and categories it
/// covers, and the named commands it exposes (e.g. `""`, `"format"`, `"check"`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Tool {
    /// Stable catalog id (the mdsf tool directory name, e.g. `"shfmt"`).
    pub name: String,
    /// Executable looked up on `PATH` (e.g. `"shfmt"`).
    pub binary: String,
    /// Free-form category tags; `formatter` / `linter` / `spell-check` are the
    /// ones poly keys off (see the `is_*` predicates).
    #[serde(default)]
    pub categories: Vec<String>,
    /// mdsf language identifiers this tool handles (e.g. `["go"]`).
    #[serde(default)]
    pub languages: Vec<String>,
    /// Named commands. The empty-string key `""` is the tool's default command.
    #[serde(default)]
    pub commands: BTreeMap<String, Command>,
    /// Upstream homepage, surfaced in diagnostics and help.
    #[serde(default)]
    pub homepage: String,
}

/// One invocation recipe for a [`Tool`]: the argument vector (which may contain
/// the [`PATH_PLACEHOLDER`]) and whether the tool reads source from stdin.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Command {
    /// Argument vector passed to the binary, verbatim apart from
    /// [`PATH_PLACEHOLDER`] substitution.
    #[serde(default)]
    pub arguments: Vec<String>,
    /// `true` when the tool consumes source on stdin rather than a file path.
    #[serde(default)]
    pub stdin: bool,
}

impl Tool {
    /// Whether the tool advertises the `formatter` category.
    pub fn is_formatter(&self) -> bool {
        self.has_category(CATEGORY_FORMATTER)
    }

    /// Whether the tool advertises the `linter` category.
    pub fn is_linter(&self) -> bool {
        self.has_category(CATEGORY_LINTER)
    }

    /// Whether the tool advertises the `spell-check` category.
    pub fn is_spell_check(&self) -> bool {
        self.has_category(CATEGORY_SPELL_CHECK)
    }

    /// Whether `category` is present in [`Tool::categories`].
    pub fn has_category(&self, category: &str) -> bool {
        self.categories.iter().any(|c| c == category)
    }

    /// Look up a command by its catalog name (`""` is the default command).
    pub fn command(&self, name: &str) -> Option<&Command> {
        self.commands.get(name)
    }

    /// The command poly should use to **format** with this tool, if any: prefers
    /// an explicit `"format"` command, then the default `""` command — but only
    /// when the tool is a formatter, so a pure linter's default command is never
    /// mistaken for a formatter.
    pub fn format_command(&self) -> Option<(&str, &Command)> {
        if !self.is_formatter() {
            return None;
        }
        self.named("format").or_else(|| self.named(""))
    }

    /// The command poly should use to **lint** with this tool, if any: prefers an
    /// explicit `"check"` / `"lint"` command, then the default `""` command, and
    /// only when the tool is a linter or spell-checker.
    pub fn lint_command(&self) -> Option<(&str, &Command)> {
        if !self.is_linter() && !self.is_spell_check() {
            return None;
        }
        self.named("check")
            .or_else(|| self.named("lint"))
            .or_else(|| self.named(""))
    }

    fn named(&self, name: &str) -> Option<(&str, &Command)> {
        self.commands
            .get_key_value(name)
            .map(|(key, command)| (key.as_str(), command))
    }
}

impl Command {
    /// Whether the argument vector references the file path via
    /// [`PATH_PLACEHOLDER`].
    pub fn uses_path(&self) -> bool {
        self.arguments
            .iter()
            .any(|argument| argument == PATH_PLACEHOLDER)
    }

    /// Concrete argv with every [`PATH_PLACEHOLDER`] replaced by `path`. Other
    /// arguments pass through unchanged.
    pub fn argv(&self, path: &str) -> Vec<String> {
        self.arguments
            .iter()
            .map(|argument| {
                if argument == PATH_PLACEHOLDER {
                    path.to_string()
                } else {
                    argument.clone()
                }
            })
            .collect()
    }
}
