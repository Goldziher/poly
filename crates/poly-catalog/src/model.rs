//! Catalog data model: one [`Tool`] per wrapped CLI, each with a map of named
//! [`Command`]s. Deserialized from the vendored `data/catalog.json` (see
//! `data/README.md` for provenance).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::recipe::Recipe;

/// The sole argument placeholder used by catalog commands. `poly` substitutes it
/// with the concrete file path at invocation time (see [`Command::argv`]).
pub const PATH_PLACEHOLDER: &str = "$PATH";

/// Placeholder expanding to every file of an invocation. `$FILES:--path` repeats
/// the flag before each file (`--path a --path b`).
pub const FILES_PLACEHOLDER: &str = "$FILES";

/// Placeholder for the group's project root, relative to the invocation's
/// working directory (always `.`).
pub const ROOT_PLACEHOLDER: &str = "$ROOT";

/// Placeholder for the directory of the file under check, relative to the
/// group's root and prefixed `./`.
pub const DIR_PLACEHOLDER: &str = "$DIR";

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
    /// Optional path-glob patterns (anchored to the repository root). When
    /// non-empty, the engine skips any file whose path does not match at least
    /// one glob. Use `**/<dir>/**/*.ext` patterns to match both relative and
    /// absolute paths. Currently used to scope `actionlint` to GitHub Actions
    /// workflow files only.
    #[serde(default)]
    pub path_globs: Vec<String>,
    /// How poly runs the tool beyond one spawn per file with an exit-code
    /// verdict. `None` for the mdsf breadth tier.
    #[serde(default)]
    pub recipe: Option<Recipe>,
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
    /// Further passes run, in order, over the same files after this one. A
    /// pipeline of in-place rewrites (palantir-java-format formats with one
    /// style, then fixes imports with another) is one command with `then`.
    #[serde(default)]
    pub then: Vec<Command>,
    /// Argv for running many files in one invocation when `arguments` itself is
    /// a single-file form (`tofu fmt -` reads one file on stdin; the batch form
    /// is `tofu fmt -write=true $FILES`, rewriting a scratch copy in place).
    #[serde(default)]
    pub batch: Option<Vec<String>>,
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
    /// Whether the argument vector takes the run's files as a list
    /// ([`FILES_PLACEHOLDER`], with or without a per-file flag).
    pub fn uses_files(&self) -> bool {
        takes_files(&self.arguments)
    }

    /// The argv for a many-file invocation: [`Command::batch`] when the command
    /// has a distinct one, else `arguments` when they already take a file list.
    pub fn batch_arguments(&self) -> Option<&[String]> {
        match &self.batch {
            Some(batch) => Some(batch),
            None if self.uses_files() => Some(&self.arguments),
            None => None,
        }
    }

    /// Whether the argument vector references the file path via
    /// [`PATH_PLACEHOLDER`].
    pub fn uses_path(&self) -> bool {
        self.arguments.iter().any(|argument| argument == PATH_PLACEHOLDER)
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

/// Whether `arguments` contains the [`FILES_PLACEHOLDER`] in either form.
pub fn takes_files(arguments: &[String]) -> bool {
    arguments
        .iter()
        .any(|argument| argument == FILES_PLACEHOLDER || argument.starts_with("$FILES:"))
}
