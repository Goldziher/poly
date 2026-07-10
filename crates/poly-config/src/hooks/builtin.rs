//! `[hooks.builtin]` — poly's first-class in-process hooks.
//!
//! Three families of builtin live here:
//!
//! - the single-tool hooks `lint` / `fmt` / `commit`, each a bare
//!   boolean (`lint = true`) or a table (`fmt = { stages = [...] }`);
//! - the [`FileSafetyHooks`] group (`file_safety`) — the pure-Rust replacement
//!   for the pre-commit-hooks file-safety block (merge-conflict markers, large
//!   files, private keys, case collisions, shebang/executable parity);
//! - the [`CargoHooks`] group (`cargo`) — whole-workspace Cargo tools (`cargo
//!   clippy` / `sort` / `machete` / `deny`), each default-on within the group
//!   and capability-probed at lowering time.
//!
//! A bare-boolean form enables the hook (or group) with default stages and, for
//! the groups, every member check turned on; a table form may set `enabled`,
//! `stages`, and per-member toggles. A table without an explicit `enabled` key
//! is treated as enabled.

use serde::{Deserialize, Deserializer};

use super::patterns::Patterns;

/// Default ceiling, in kibibytes, for [`FileSafetyHooks::added_large_files`].
pub const DEFAULT_MAX_ADDED_FILE_KB: u64 = 500;

/// `[hooks.builtin]` — poly's first-class in-process hooks.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct BuiltinHooks {
    /// The `lint` linter hook.
    pub lint: BuiltinHook,
    /// The `fmt` formatter hook.
    pub fmt: BuiltinHook,
    /// The `poly commit` message-lint hook.
    pub commit: BuiltinHook,
    /// The pure-Rust file-safety check group (`file_safety`).
    pub file_safety: FileSafetyHooks,
    /// The whole-workspace Cargo tool group (`cargo`).
    ///
    /// `None` when the `cargo` key is absent — distinct from an explicit
    /// `cargo = false`. With a `[hooks]` section present, an absent key means
    /// the group runs by default (capability-probed); an explicit value wins.
    pub cargo: Option<CargoHooks>,
}

/// One builtin hook. Accepts either a bare boolean (`lint = true`) or a
/// table (`fmt = { stages = ["pre-commit"] }`); a table without an explicit
/// `enabled` key is treated as enabled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuiltinHook {
    /// Whether this builtin hook is active.
    pub enabled: bool,
    /// Stages this hook runs in; empty means inherit [`super::HooksConfig::stages`].
    pub stages: Vec<String>,
    /// File include glob(s); `None` matches every candidate file.
    pub files: Option<Patterns>,
    /// File exclude glob(s) filtered from the matched set before the hook runs.
    pub exclude: Option<Patterns>,
}

/// On-disk form of a builtin hook: bare toggle or a table.
#[derive(Deserialize)]
#[serde(untagged)]
enum BuiltinHookRepr {
    Toggle(bool),
    Table(BuiltinHookTable),
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct BuiltinHookTable {
    enabled: Option<bool>,
    stages: Vec<String>,
    files: Option<Patterns>,
    exclude: Option<Patterns>,
}

impl<'de> Deserialize<'de> for BuiltinHook {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match BuiltinHookRepr::deserialize(deserializer)? {
            BuiltinHookRepr::Toggle(enabled) => Ok(BuiltinHook {
                enabled,
                stages: Vec::new(),
                files: None,
                exclude: None,
            }),
            BuiltinHookRepr::Table(table) => Ok(BuiltinHook {
                enabled: table.enabled.unwrap_or(true),
                stages: table.stages,
                files: table.files,
                exclude: table.exclude,
            }),
        }
    }
}

/// The `file_safety` builtin group — pure-Rust file-safety checks.
///
/// Enable the whole group with defaults via `file_safety = true`, or tune it
/// with a table:
///
/// ```toml
/// [hooks.builtin.file_safety]
/// stages = ["pre-commit"]
/// max_added_file_kb = 1000
/// private_key = false           # opt a single check out
/// ```
///
/// Every member check defaults to on when the group is enabled; the group
/// itself is off by default (like the other builtins).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSafetyHooks {
    /// Whether the file-safety group is active.
    pub enabled: bool,
    /// Stages this group runs in; empty means inherit [`super::HooksConfig::stages`].
    pub stages: Vec<String>,
    /// File include glob(s); `None` matches every candidate file.
    pub files: Option<Patterns>,
    /// File exclude glob(s) filtered from the matched set before the checks run.
    pub exclude: Option<Patterns>,
    /// Size ceiling, in kibibytes, for the large-file check.
    pub max_added_file_kb: u64,
    /// Reject files containing git merge-conflict markers.
    pub merge_conflict: bool,
    /// Reject files larger than [`Self::max_added_file_kb`].
    pub added_large_files: bool,
    /// Reject files containing a private-key header.
    pub private_key: bool,
    /// Reject paths that collide case-insensitively.
    pub case_conflict: bool,
    /// Require executable files to start with a `#!` shebang.
    pub executables_have_shebangs: bool,
    /// Require files starting with `#!` to be executable.
    pub shebang_scripts_are_executable: bool,
}

impl Default for FileSafetyHooks {
    fn default() -> Self {
        Self {
            enabled: false,
            stages: Vec::new(),
            files: None,
            exclude: None,
            max_added_file_kb: DEFAULT_MAX_ADDED_FILE_KB,
            merge_conflict: true,
            added_large_files: true,
            private_key: true,
            case_conflict: true,
            executables_have_shebangs: true,
            shebang_scripts_are_executable: true,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FileSafetyRepr {
    Toggle(bool),
    Table(FileSafetyTable),
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct FileSafetyTable {
    enabled: Option<bool>,
    stages: Vec<String>,
    files: Option<Patterns>,
    exclude: Option<Patterns>,
    max_added_file_kb: Option<u64>,
    merge_conflict: Option<bool>,
    added_large_files: Option<bool>,
    private_key: Option<bool>,
    case_conflict: Option<bool>,
    executables_have_shebangs: Option<bool>,
    shebang_scripts_are_executable: Option<bool>,
}

impl<'de> Deserialize<'de> for FileSafetyHooks {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match FileSafetyRepr::deserialize(deserializer)? {
            FileSafetyRepr::Toggle(enabled) => Ok(FileSafetyHooks {
                enabled,
                ..FileSafetyHooks::default()
            }),
            FileSafetyRepr::Table(table) => Ok(FileSafetyHooks {
                enabled: table.enabled.unwrap_or(true),
                stages: table.stages,
                files: table.files,
                exclude: table.exclude,
                max_added_file_kb: table.max_added_file_kb.unwrap_or(DEFAULT_MAX_ADDED_FILE_KB),
                merge_conflict: table.merge_conflict.unwrap_or(true),
                added_large_files: table.added_large_files.unwrap_or(true),
                private_key: table.private_key.unwrap_or(true),
                case_conflict: table.case_conflict.unwrap_or(true),
                executables_have_shebangs: table.executables_have_shebangs.unwrap_or(true),
                shebang_scripts_are_executable: table.shebang_scripts_are_executable.unwrap_or(true),
            }),
        }
    }
}

/// The `cargo` builtin group — whole-workspace Cargo tools.
///
/// Enable the group with defaults via `cargo = true`, or tune it with a table:
///
/// ```toml
/// [hooks.builtin.cargo]
/// stages = ["pre-commit"]
/// machete = false               # opt a single tool out
/// ```
///
/// Each member tool defaults to on when the group is enabled; the group itself
/// is off by default. Member tools are capability-probed at lowering time and
/// silently skipped when the tool is not on `PATH`, so enabling the group is
/// safe even in a repository that only ships some of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoHooks {
    /// Whether the Cargo tool group is active.
    pub enabled: bool,
    /// Stages this group runs in; empty means inherit [`super::HooksConfig::stages`].
    pub stages: Vec<String>,
    /// Run `cargo clippy` over the workspace.
    pub clippy: bool,
    /// Run `cargo sort --check` over the workspace.
    pub sort: bool,
    /// Run `cargo machete` over the workspace.
    pub machete: bool,
    /// Run `cargo deny check` over the workspace.
    pub deny: bool,
    /// Override the flags passed to `cargo clippy` (the part before `-- -D warnings`).
    ///
    /// When `None`, the default `--workspace --all-targets` is used. When `Some`, the
    /// provided list **replaces** the default flags entirely; `-- -D warnings` is always
    /// appended to preserve the strict-warnings policy.
    ///
    /// Example: `clippy_args = ["--workspace", "--exclude=crawlberg-php", "--all-features"]`
    pub clippy_args: Option<Vec<String>>,
    /// Whether the group's result cache is active (default on).
    ///
    /// When on (and `[cache.results] hooks` is not `off`), each Cargo tool is
    /// keyed on the Rust source + manifest inputs, so a commit that changes no
    /// Rust skips the whole group. Set `cache = false` to force every run.
    pub cache: bool,
    /// Whether the group runs in the whole-project phase of `poly lint` (default
    /// on). Set `lint = false` to keep clippy/sort/machete/deny as a git-hook
    /// gate while excluding them from `poly lint`, whose lightweight checkout may
    /// be unable to compile the workspace. The `[lint] workspace = false` switch
    /// disables the phase wholesale; this is the per-group opt-out.
    pub lint: bool,
}

impl Default for CargoHooks {
    fn default() -> Self {
        Self {
            enabled: false,
            stages: Vec::new(),
            clippy: true,
            sort: true,
            machete: true,
            deny: true,
            clippy_args: None,
            cache: true,
            lint: true,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CargoRepr {
    Toggle(bool),
    Table(CargoTable),
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct CargoTable {
    enabled: Option<bool>,
    stages: Vec<String>,
    clippy: Option<bool>,
    sort: Option<bool>,
    machete: Option<bool>,
    deny: Option<bool>,
    clippy_args: Option<Vec<String>>,
    cache: Option<bool>,
    lint: Option<bool>,
}

impl<'de> Deserialize<'de> for CargoHooks {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match CargoRepr::deserialize(deserializer)? {
            CargoRepr::Toggle(enabled) => Ok(CargoHooks {
                enabled,
                ..CargoHooks::default()
            }),
            CargoRepr::Table(table) => Ok(CargoHooks {
                enabled: table.enabled.unwrap_or(true),
                stages: table.stages,
                clippy: table.clippy.unwrap_or(true),
                sort: table.sort.unwrap_or(true),
                machete: table.machete.unwrap_or(true),
                deny: table.deny.unwrap_or(true),
                clippy_args: table.clippy_args,
                cache: table.cache.unwrap_or(true),
                lint: table.lint.unwrap_or(true),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_toggle_enables_with_no_stages() {
        let hooks: BuiltinHooks = toml::from_str("lint = true").unwrap();
        assert!(hooks.lint.enabled);
        assert!(hooks.lint.stages.is_empty());
        assert!(!hooks.fmt.enabled);
    }

    #[test]
    fn table_without_enabled_is_enabled() {
        let hooks: BuiltinHooks = toml::from_str(r#"fmt = { stages = ["pre-commit"] }"#).unwrap();
        assert!(hooks.fmt.enabled);
        assert_eq!(hooks.fmt.stages, vec!["pre-commit".to_string()]);
    }

    #[test]
    fn legacy_hook_keys_are_rejected() {
        assert!(
            toml::from_str::<BuiltinHooks>("polylint = true").is_err(),
            "legacy `polylint` key must be rejected"
        );
        assert!(
            toml::from_str::<BuiltinHooks>("polyfmt = true").is_err(),
            "legacy `polyfmt` key must be rejected"
        );
    }

    #[test]
    fn table_with_explicit_disable() {
        let hooks: BuiltinHooks = toml::from_str("commit = { enabled = false }").unwrap();
        assert!(!hooks.commit.enabled);
    }

    #[test]
    fn builtin_table_parses_files_and_exclude_globs() {
        let hooks: BuiltinHooks = toml::from_str(
            r#"
[lint]
exclude = ["**/tags.rs", ".ai-rulez/**"]
[fmt]
files = "**/*.rs"
"#,
        )
        .unwrap();
        assert!(hooks.lint.enabled);
        assert_eq!(
            hooks.lint.exclude.as_ref().map(Patterns::as_slice),
            Some(&["**/tags.rs".to_string(), ".ai-rulez/**".to_string()][..])
        );
        assert!(hooks.lint.files.is_none());
        assert_eq!(
            hooks.fmt.files.as_ref().map(Patterns::as_slice),
            Some(&["**/*.rs".to_string()][..])
        );
    }

    #[test]
    fn file_safety_table_parses_exclude_glob() {
        let hooks: BuiltinHooks = toml::from_str(
            r#"
[file_safety]
exclude = "crates/poly-cli/src/hooks/checks.rs"
"#,
        )
        .unwrap();
        assert!(hooks.file_safety.enabled);
        assert_eq!(
            hooks.file_safety.exclude.as_ref().map(Patterns::as_slice),
            Some(&["crates/poly-cli/src/hooks/checks.rs".to_string()][..])
        );
    }

    #[test]
    fn file_safety_off_and_cargo_absent_by_default() {
        let hooks = BuiltinHooks::default();
        assert!(!hooks.file_safety.enabled);
        assert!(hooks.cargo.is_none());
        assert!(hooks.file_safety.merge_conflict);
        assert!(CargoHooks::default().clippy);
        assert_eq!(hooks.file_safety.max_added_file_kb, DEFAULT_MAX_ADDED_FILE_KB);
    }

    #[test]
    fn file_safety_bare_toggle_enables_every_check() {
        let hooks: BuiltinHooks = toml::from_str("file_safety = true").unwrap();
        let safety = &hooks.file_safety;
        assert!(safety.enabled);
        assert!(safety.merge_conflict);
        assert!(safety.added_large_files);
        assert!(safety.private_key);
        assert!(safety.case_conflict);
        assert!(safety.executables_have_shebangs);
        assert!(safety.shebang_scripts_are_executable);
        assert_eq!(safety.max_added_file_kb, DEFAULT_MAX_ADDED_FILE_KB);
    }

    #[test]
    fn file_safety_table_opts_out_a_single_check_and_keeps_the_rest_on() {
        let hooks: BuiltinHooks = toml::from_str(
            r#"
[file_safety]
max_added_file_kb = 1000
private_key = false
"#,
        )
        .unwrap();
        let safety = &hooks.file_safety;
        assert!(safety.enabled);
        assert_eq!(safety.max_added_file_kb, 1000);
        assert!(!safety.private_key);
        assert!(safety.merge_conflict);
        assert!(safety.added_large_files);
    }

    #[test]
    fn file_safety_table_with_stages_carries_them() {
        let hooks: BuiltinHooks = toml::from_str(r#"file_safety = { stages = ["pre-commit", "pre-push"] }"#).unwrap();
        assert!(hooks.file_safety.enabled);
        assert_eq!(hooks.file_safety.stages, vec!["pre-commit", "pre-push"]);
    }

    #[test]
    fn file_safety_rejects_unknown_keys() {
        let result: Result<BuiltinHooks, _> = toml::from_str("[file_safety]\nbogus = true\n");
        assert!(result.is_err(), "deny_unknown_fields must reject `bogus`");
    }

    #[test]
    fn cargo_bare_toggle_enables_every_tool() {
        let hooks: BuiltinHooks = toml::from_str("cargo = true").unwrap();
        let cargo = hooks.cargo.as_ref().expect("cargo present");
        assert!(cargo.enabled);
        assert!(cargo.clippy);
        assert!(cargo.sort);
        assert!(cargo.machete);
        assert!(cargo.deny);
    }

    #[test]
    fn cargo_table_opts_out_a_single_tool() {
        let hooks: BuiltinHooks = toml::from_str(
            r#"
[cargo]
machete = false
"#,
        )
        .unwrap();
        let cargo = hooks.cargo.as_ref().expect("cargo present");
        assert!(cargo.enabled);
        assert!(!cargo.machete);
        assert!(cargo.clippy);
        assert!(cargo.sort);
        assert!(cargo.deny);
    }

    #[test]
    fn cargo_table_clippy_args_replaces_default_flags() {
        let hooks: BuiltinHooks =
            toml::from_str(r#"cargo = { clippy_args = ["--workspace", "--exclude=foo", "--all-features"] }"#).unwrap();
        let cargo = hooks.cargo.expect("cargo table present");
        assert!(cargo.clippy, "clippy is on by default in a table");
        assert_eq!(
            cargo.clippy_args.as_deref(),
            Some(
                &[
                    "--workspace".to_string(),
                    "--exclude=foo".to_string(),
                    "--all-features".to_string()
                ][..]
            ),
        );
    }

    #[test]
    fn cargo_default_has_no_clippy_args_override() {
        let hooks: BuiltinHooks = toml::from_str("cargo = true").unwrap();
        let cargo = hooks.cargo.expect("cargo enabled");
        assert!(cargo.clippy_args.is_none(), "no override → default flags apply");
    }

    #[test]
    fn cargo_table_with_explicit_disable() {
        let hooks: BuiltinHooks = toml::from_str("cargo = { enabled = false }").unwrap();
        assert!(!hooks.cargo.expect("cargo present").enabled);
    }

    #[test]
    fn cargo_lint_defaults_on_and_opts_out() {
        let default: BuiltinHooks = toml::from_str("cargo = true").unwrap();
        assert!(
            default.cargo.expect("cargo present").lint,
            "lint participation is on by default"
        );

        let opted_out: BuiltinHooks = toml::from_str("cargo = { lint = false }").unwrap();
        let cargo = opted_out.cargo.expect("cargo present");
        assert!(!cargo.lint, "lint = false excludes the group from `poly lint`");
        assert!(cargo.enabled, "the group still runs as a git hook");
        assert!(cargo.clippy && cargo.sort && cargo.machete && cargo.deny);
    }
}
