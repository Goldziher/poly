//! Catalog-driven formatter engine (ADR 0013).
//!
//! Wraps any [`poly_catalog`] formatter as an [`Engine`], so a user can opt a
//! vendored catalog tool in from `poly.toml` (`[tools.<name>] enabled = true`)
//! and have `poly fmt` run it per file. Two execution models, taken from the
//! catalog command:
//!
//! - **stdin** (`stdin = true`) — source is piped to the tool and the formatted
//!   result read from its stdout.
//! - **path** (the common case) — source is written to a temp file, the `$PATH`
//!   placeholder in the argv is substituted with that path, the tool rewrites
//!   the file in place, and the result is read back.
//!
//! The engine is **capability-probed**: when the tool's binary is absent from
//! `PATH` it is a no-op ([`FormatOutput::Unchanged`]), so a missing tool
//! degrades gracefully rather than erroring. It is registered only for tools a
//! user has explicitly enabled, and routed by `crate::registry` (hence
//! [`Engine::languages`] returns an empty slice).
//!
//! A catalog tool can be wired as **either** a formatter ([`CatalogToolEngine::format_engine`])
//! **or** a linter ([`CatalogToolEngine::lint_engine`]). Catalog linting is a
//! best-effort, breadth-tier mechanism: it runs the tool's lint command per file
//! and maps a non-zero exit to a single file-level [`Diagnostic`] (no span, no
//! rule code). Structured, per-rule diagnostics remain the job of the curated
//! native backends — the catalog tier trades fidelity for breadth.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use anyhow::Context;
use globset::{Glob, GlobSet, GlobSetBuilder};
use poly_catalog::{PATH_PLACEHOLDER, Tool};

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile};
use crate::language::Language;

/// Argv flags that mutate the file rather than only reporting on it. A lint
/// command carrying any of these is **rejected** by [`CatalogToolEngine::lint_engine`]:
/// running a mutating command as a linter would silently rewrite the user's
/// files, so such a tool is skipped (no lint engine) rather than run.
const MUTATING_FLAGS: &[&str] = &["--fix", "--write", "-w", "-i"];

/// Maximum number of bytes of a failing linter's output surfaced in the
/// file-level diagnostic message, so a chatty tool cannot flood the report.
const MAX_SNIPPET_LEN: usize = 2000;

/// Whether a catalog engine formats or lints. A single [`CatalogToolEngine`]
/// serves exactly one role, selected at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Rewrites the file (`poly fmt`).
    Format,
    /// Reports findings without mutating the file (`poly lint`).
    Lint,
}

/// Inputs at or below this size are written to the child's stdin inline (they
/// fit the OS pipe buffer, so the write cannot block); larger inputs use a
/// dedicated writer thread to avoid a pipe-buffer deadlock. Mirrors the
/// native-tool formatter's policy.
const STDIN_INLINE_LIMIT: usize = 8 * 1024;

/// Per-process cache of `binary name -> Some(version) | None (absent)`.
fn probe_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Probe a binary's presence (and best-effort version), memoised for the process
/// lifetime. `None` means the binary is not on `PATH`.
fn probe_binary(binary: &str) -> Option<String> {
    if let Some(cached) = probe_cache().lock().expect("probe cache poisoned").get(binary) {
        return cached.clone();
    }
    let result = which::which(binary).ok().map(|_| version_of(binary));
    probe_cache()
        .lock()
        .expect("probe cache poisoned")
        .insert(binary.to_string(), result.clone());
    result
}

/// Best-effort `--version` string for a present binary; `"found"` when the tool
/// does not answer `--version`.
fn version_of(binary: &str) -> String {
    Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|out| {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if stdout.is_empty() {
                String::from_utf8_lossy(&out.stderr).trim().to_string()
            } else {
                stdout
            }
        })
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| "found".to_string())
}

/// A catalog tool wired as an [`Engine`] for one enabled `[tools.<name>]`,
/// serving either the format or the lint role.
pub struct CatalogToolEngine {
    tool: &'static Tool,
    /// Whether this engine formats or lints.
    mode: Mode,
    /// Resolved argv (catalog command's arguments, or the user's `args` override).
    arguments: Vec<String>,
    /// Whether the tool reads source on stdin (vs. a `$PATH` file).
    stdin: bool,
    /// Cache-key version: folds the probed binary version, the mode, the resolved
    /// argv, and the stdin mode so any change invalidates stale cached results.
    version: String,
    /// Extra environment variables injected when spawning the tool process.
    env: BTreeMap<String, String>,
    /// Working directory override for the spawned process; `None` means the
    /// process inherits the caller's cwd (the repo root at runtime).
    root: Option<PathBuf>,
    /// Pre-compiled glob set for fast matching. Built from the catalog tool's
    /// `path_globs` at construction time; `None` when `path_globs` is empty.
    /// When `Some`, only files whose path matches at least one pattern are linted.
    path_glob_set: Option<GlobSet>,
}

impl CatalogToolEngine {
    /// Build a formatter engine for `tool`. `command_name` selects the catalog
    /// command (`None` → the tool's [`Tool::format_command`]); `args_override`
    /// replaces the command's argv when present. `env` and `root` are forwarded
    /// to the spawned process. Returns `None` when the tool exposes no usable
    /// format command.
    pub fn format_engine(
        tool: &'static Tool,
        command_name: Option<&str>,
        args_override: Option<&[String]>,
        env: BTreeMap<String, String>,
        root: Option<PathBuf>,
    ) -> Option<Self> {
        let command = match command_name {
            Some(name) => tool.command(name)?,
            None => tool.format_command()?.1,
        };
        let arguments = args_override
            .map(<[String]>::to_vec)
            .unwrap_or_else(|| command.arguments.clone());
        Some(Self::build(tool, Mode::Format, arguments, command.stdin, env, root))
    }

    /// Build a linter engine for `tool`. `command_name` selects the catalog
    /// command (`None` → the tool's [`Tool::lint_command`]); `args_override`
    /// replaces the command's argv when present. `env` and `root` are forwarded
    /// to the spawned process.
    ///
    /// Returns `None` when the tool exposes no usable lint command **or** when the
    /// resolved argv is mutating (contains any of [`MUTATING_FLAGS`]): a command
    /// that rewrites the file must never run as a linter, since the runner does
    /// not expect a lint pass to touch the file. Such a tool is simply not
    /// registered as a linter (it can still be wired as a formatter).
    pub fn lint_engine(
        tool: &'static Tool,
        command_name: Option<&str>,
        args_override: Option<&[String]>,
        env: BTreeMap<String, String>,
        root: Option<PathBuf>,
    ) -> Option<Self> {
        let command = match command_name {
            Some(name) => tool.command(name)?,
            None => tool.lint_command()?.1,
        };
        let arguments = args_override
            .map(<[String]>::to_vec)
            .unwrap_or_else(|| command.arguments.clone());
        if is_mutating(&arguments) {
            return None;
        }
        Some(Self::build(tool, Mode::Lint, arguments, command.stdin, env, root))
    }

    /// Shared constructor for both roles: probes the binary and folds the role,
    /// argv, stdin mode, env, and root into the cache-key version.
    fn build(
        tool: &'static Tool,
        mode: Mode,
        arguments: Vec<String>,
        stdin: bool,
        env: BTreeMap<String, String>,
        root: Option<PathBuf>,
    ) -> Self {
        let probe = probe_binary(&tool.binary);
        let path_globs = tool.path_globs.clone();
        let path_glob_set = if path_globs.is_empty() {
            None
        } else {
            let mut builder = GlobSetBuilder::new();
            for pattern in &path_globs {
                if let Ok(glob) = Glob::new(pattern) {
                    builder.add(glob);
                }
            }
            builder.build().ok()
        };
        let version = format!(
            "catalog:{}:{}:mode={mode:?}:stdin={stdin}:args={arguments:?}:env={env:?}:root={root:?}:path_globs={path_globs:?}",
            tool.name,
            probe.as_deref().unwrap_or("absent"),
        );
        CatalogToolEngine {
            tool,
            mode,
            arguments,
            stdin,
            version,
            env,
            root,
            path_glob_set,
        }
    }

    /// Whether `mode` is the lint role (a small helper kept for readability at
    /// the call sites).
    fn is_lint(&self) -> bool {
        self.mode == Mode::Lint
    }

    /// Substitute the `$PATH` placeholder in the resolved argv with `path`.
    fn argv_with_path(&self, path: &str) -> Vec<String> {
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

    /// Build a [`Command`] for `binary` pre-configured with the engine's `env`
    /// and `root` (if any), so callers only need to append argv / stdio.
    fn base_command(&self, binary: &str) -> Command {
        let mut cmd = Command::new(binary);
        cmd.envs(&self.env);
        if let Some(root) = &self.root {
            cmd.current_dir(root);
        }
        cmd
    }
}

impl Engine for CatalogToolEngine {
    fn name(&self) -> &'static str {
        // The catalog is a process-lifetime static, so its tool names are
        // `'static` — satisfying the trait without leaking.
        &self.tool.name
    }

    fn languages(&self) -> &'static [Language] {
        // Routing is decided by the registry from `[tools]` config, not by this
        // slice (see module docs).
        &[]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: self.is_lint(),
            format: !self.is_lint(),
            fix: false,
        }
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn format(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        // A lint engine never formats (declared via `capabilities`); guard anyway.
        if self.is_lint() {
            return Ok(FormatOutput::Unchanged);
        }
        // Absent tool → no-op (graceful degradation, never an error).
        if probe_binary(&self.tool.binary).is_none() {
            return Ok(FormatOutput::Unchanged);
        }
        if self.stdin {
            self.format_via_stdin(src)
        } else {
            self.format_via_path(src)
        }
    }

    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        // A format engine never lints (declared via `capabilities`); guard anyway.
        if !self.is_lint() {
            return Ok(Vec::new());
        }
        // Path-glob filter: skip files that don't match the catalog's declared
        // scope (e.g. actionlint only applies to .github/workflows/ files).
        if let Some(ref set) = self.path_glob_set
            && !set.is_match(&src.path)
        {
            return Ok(Vec::new());
        }
        // Absent tool → no findings (graceful degradation, never an error).
        if probe_binary(&self.tool.binary).is_none() {
            return Ok(Vec::new());
        }
        let outcome = if self.stdin {
            self.lint_via_stdin(src)?
        } else {
            self.lint_via_path(src)?
        };
        Ok(self.diagnostics_for(outcome))
    }
}

impl CatalogToolEngine {
    /// stdin → stdout formatting.
    fn format_via_stdin(&self, src: &SourceFile) -> anyhow::Result<FormatOutput> {
        let binary = &self.tool.binary;
        let argv = self.argv_with_path("-");
        let mut child = self
            .base_command(binary)
            .args(&argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn '{binary}'"))?;

        let content = Arc::clone(&src.content);
        let mut stdin_handle = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("'{binary}' stdin pipe was not created"))?;
        // Small inputs fit the pipe buffer (write inline); larger inputs use a
        // writer thread to avoid deadlocking against `wait_with_output`.
        let writer = if content.len() <= STDIN_INLINE_LIMIT {
            let result = stdin_handle.write_all(content.as_bytes());
            drop(stdin_handle);
            WriteOutcome::Inline(result)
        } else {
            WriteOutcome::Thread(thread::spawn(move || stdin_handle.write_all(content.as_bytes())))
        };

        let output = child
            .wait_with_output()
            .with_context(|| format!("'{binary}' wait_with_output failed"))?;

        // Non-zero exit (e.g. a syntax error) → leave the file untouched. A
        // broken-pipe write error in that case is expected, so discard it.
        if !output.status.success() {
            if let WriteOutcome::Thread(handle) = writer {
                let _ = handle.join();
            }
            return Ok(FormatOutput::Unchanged);
        }
        match writer {
            WriteOutcome::Inline(result) => {
                result.with_context(|| format!("failed to write to '{binary}' stdin"))?;
            }
            WriteOutcome::Thread(handle) => {
                handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("stdin writer panicked for '{binary}'"))?
                    .with_context(|| format!("failed to write to '{binary}' stdin"))?;
            }
        }

        let formatted =
            String::from_utf8(output.stdout).with_context(|| format!("'{binary}' produced non-UTF-8 output"))?;
        Ok(diff_output(formatted, src))
    }

    /// Temp-file (`$PATH`) formatting: write source to a temp file, run the tool
    /// (which rewrites it in place), and read it back.
    fn format_via_path(&self, src: &SourceFile) -> anyhow::Result<FormatOutput> {
        let binary = &self.tool.binary;
        let extension = src.path.extension().and_then(|ext| ext.to_str()).unwrap_or("txt");
        let mut temp = tempfile::Builder::new()
            .prefix("poly-catalog-")
            .suffix(&format!(".{extension}"))
            .tempfile()
            .context("creating temp file for catalog tool")?;
        temp.write_all(src.content.as_bytes())
            .context("writing source to temp file")?;
        temp.flush().context("flushing temp file")?;

        let path = temp.path().to_string_lossy().to_string();
        let argv = self.argv_with_path(&path);
        let output = self
            .base_command(binary)
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .with_context(|| format!("failed to run '{binary}'"))?;

        // Non-zero exit → tool rejected the input; never corrupt the file.
        if !output.status.success() {
            return Ok(FormatOutput::Unchanged);
        }
        let formatted = std::fs::read_to_string(temp.path())
            .with_context(|| format!("reading '{binary}' output back from temp file"))?;
        Ok(diff_output(formatted, src))
    }

    /// Lint via stdin: pipe the source to the tool and capture its exit status
    /// and combined output. The tool never sees (or rewrites) a real file.
    fn lint_via_stdin(&self, src: &SourceFile) -> anyhow::Result<LintOutcome> {
        let binary = &self.tool.binary;
        let argv = self.argv_with_path("-");
        let mut child = self
            .base_command(binary)
            .args(&argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn '{binary}'"))?;

        let content = Arc::clone(&src.content);
        let mut stdin_handle = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("'{binary}' stdin pipe was not created"))?;
        // Mirror the formatter's pipe-buffer policy: inline writes for small
        // inputs, a writer thread for large ones (avoids a pipe deadlock).
        let writer = if content.len() <= STDIN_INLINE_LIMIT {
            let result = stdin_handle.write_all(content.as_bytes());
            drop(stdin_handle);
            WriteOutcome::Inline(result)
        } else {
            WriteOutcome::Thread(thread::spawn(move || stdin_handle.write_all(content.as_bytes())))
        };

        let output = child
            .wait_with_output()
            .with_context(|| format!("'{binary}' wait_with_output failed"))?;
        // A broken-pipe write error is expected when the tool exits early; the
        // exit status is what matters for linting, so the write result is ignored.
        if let WriteOutcome::Thread(handle) = writer {
            let _ = handle.join();
        }
        Ok(LintOutcome::new(
            output.status.success(),
            &output.stdout,
            &output.stderr,
        ))
    }

    /// Lint via a temp file (`$PATH`): write the source to a temp file the tool
    /// only reads, run it, and capture its exit status and combined output.
    fn lint_via_path(&self, src: &SourceFile) -> anyhow::Result<LintOutcome> {
        let binary = &self.tool.binary;
        let extension = src.path.extension().and_then(|ext| ext.to_str()).unwrap_or("txt");
        let mut temp = tempfile::Builder::new()
            .prefix("poly-catalog-")
            .suffix(&format!(".{extension}"))
            .tempfile()
            .context("creating temp file for catalog tool")?;
        temp.write_all(src.content.as_bytes())
            .context("writing source to temp file")?;
        temp.flush().context("flushing temp file")?;

        let path = temp.path().to_string_lossy().to_string();
        let argv = self.argv_with_path(&path);
        let output = self
            .base_command(binary)
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("failed to run '{binary}'"))?;
        Ok(LintOutcome::new(
            output.status.success(),
            &output.stdout,
            &output.stderr,
        ))
    }

    /// Map a [`LintOutcome`] to diagnostics: a passing run yields none; a failing
    /// run yields exactly one file-level [`Severity::Warning`] carrying a trimmed
    /// snippet of the tool's output (no span, no rule code — breadth-tier
    /// fidelity).
    fn diagnostics_for(&self, outcome: LintOutcome) -> Vec<Diagnostic> {
        if outcome.success {
            return Vec::new();
        }
        let snippet = if outcome.message.is_empty() {
            format!("{} reported a problem", self.tool.name)
        } else {
            outcome.message
        };
        vec![Diagnostic {
            engine: self.tool.name.clone(),
            code: None,
            severity: Severity::Warning,
            title: snippet,
            description: None,
            span: None,
            url: None,
            fix: Vec::new(),
            metadata: std::collections::BTreeMap::new(),
        }]
    }
}

/// The captured result of one catalog lint invocation.
struct LintOutcome {
    /// Whether the tool exited zero (no findings).
    success: bool,
    /// Trimmed, length-capped snippet of the tool's stdout (falling back to
    /// stderr), surfaced in the file-level diagnostic.
    message: String,
}

impl LintOutcome {
    fn new(success: bool, stdout: &[u8], stderr: &[u8]) -> Self {
        let stdout = String::from_utf8_lossy(stdout);
        let trimmed = stdout.trim();
        let chosen = if trimmed.is_empty() {
            String::from_utf8_lossy(stderr).trim().to_string()
        } else {
            trimmed.to_string()
        };
        let message = if chosen.len() > MAX_SNIPPET_LEN {
            // Truncate on a char boundary so the snippet stays valid UTF-8.
            let mut end = MAX_SNIPPET_LEN;
            while !chosen.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…", &chosen[..end])
        } else {
            chosen
        };
        LintOutcome { success, message }
    }
}

/// Whether `arguments` contain any [`MUTATING_FLAGS`] token — i.e. the command
/// would rewrite the file rather than only report on it.
fn is_mutating(arguments: &[String]) -> bool {
    arguments
        .iter()
        .any(|argument| MUTATING_FLAGS.contains(&argument.as_str()))
}

/// How stdin was fed to the child (see [`CatalogToolEngine::format_via_stdin`]).
enum WriteOutcome {
    Inline(std::io::Result<()>),
    Thread(thread::JoinHandle<std::io::Result<()>>),
}

/// `Unchanged` when `formatted` equals the source byte-for-byte, else
/// `Formatted`.
fn diff_output(formatted: String, src: &SourceFile) -> FormatOutput {
    if formatted == src.content.as_ref() {
        FormatOutput::Unchanged
    } else {
        FormatOutput::Formatted(formatted)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use poly_catalog::{Catalog, Command as CatalogCommand};

    use super::*;
    use crate::config::{EngineConfig, GlobalDefaults};

    /// Build a leaked `&'static Tool` for a single-command catalog tool, so the
    /// `&'static Tool` contract is satisfied without a real catalog entry.
    fn leak_tool(name: &str, binary: &str, category: &str, arguments: Vec<String>) -> &'static Tool {
        Box::leak(Box::new(Tool {
            name: name.to_string(),
            binary: binary.to_string(),
            categories: vec![category.to_string()],
            languages: vec!["text".to_string()],
            commands: BTreeMap::from([(
                String::new(),
                CatalogCommand {
                    arguments,
                    stdin: false,
                },
            )]),
            homepage: String::new(),
            path_globs: vec![],
        }))
    }

    fn make_src(path: &str, content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            language: Language::Other("test".to_string()),
            content: content.into(),
        }
    }

    fn cfg() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::Table::new(),
        }
    }

    /// Convenience wrapper for tests: build an engine with empty env and no root.
    fn format_engine_default(
        tool: &'static Tool,
        command_name: Option<&str>,
        args_override: Option<&[String]>,
    ) -> Option<CatalogToolEngine> {
        CatalogToolEngine::format_engine(tool, command_name, args_override, BTreeMap::new(), None)
    }

    /// Convenience wrapper for tests: build a lint engine with empty env and no root.
    fn lint_engine_default(
        tool: &'static Tool,
        command_name: Option<&str>,
        args_override: Option<&[String]>,
    ) -> Option<CatalogToolEngine> {
        CatalogToolEngine::lint_engine(tool, command_name, args_override, BTreeMap::new(), None)
    }

    #[test]
    fn format_engine_builds_for_a_catalog_formatter() {
        let tool = Catalog::get().tool("shfmt").expect("shfmt in catalog");
        let engine = format_engine_default(tool, None, None).expect("shfmt exposes a format command");
        assert_eq!(engine.name(), "shfmt");
        assert!(engine.capabilities().format);
        assert!(!engine.capabilities().lint);
        assert!(engine.version().contains("shfmt"));
    }

    #[test]
    fn format_engine_none_for_pure_linter() {
        // shellcheck is lint-only; it has no format command.
        if let Some(tool) = Catalog::get().tool("shellcheck") {
            assert!(format_engine_default(tool, None, None).is_none());
        }
    }

    #[test]
    fn args_override_replaces_catalog_argv() {
        let tool = Catalog::get().tool("shfmt").expect("shfmt in catalog");
        let engine = format_engine_default(tool, None, Some(&["--custom".to_string()])).unwrap();
        assert_eq!(engine.arguments, vec!["--custom".to_string()]);
        assert!(engine.version().contains("--custom"));
    }

    #[test]
    fn argv_substitutes_path_placeholder() {
        let tool = Catalog::get().tool("gofmt").expect("gofmt in catalog");
        let engine = format_engine_default(tool, None, None).unwrap();
        let argv = engine.argv_with_path("/tmp/x.go");
        assert!(argv.iter().any(|a| a == "/tmp/x.go"));
        assert!(!argv.iter().any(|a| a == PATH_PLACEHOLDER));
    }

    #[test]
    fn lint_engine_rejects_a_mutating_command() {
        // A `--fix` command would rewrite files; it must never be wired as a
        // linter, regardless of which mutating flag is present.
        for flag in ["--fix", "--write", "-w", "-i"] {
            let tool = leak_tool(
                "fakefixer",
                "true",
                "linter",
                vec![flag.to_string(), PATH_PLACEHOLDER.to_string()],
            );
            assert!(
                lint_engine_default(tool, None, None).is_none(),
                "mutating flag `{flag}` must be rejected as a linter"
            );
        }
    }

    #[test]
    fn lint_engine_rejects_a_mutating_args_override() {
        // The guard applies to the user's `args` override too, not just the
        // catalog's own argv.
        let tool = leak_tool("fakelint", "true", "linter", vec![PATH_PLACEHOLDER.to_string()]);
        assert!(lint_engine_default(tool, None, Some(&["--fix".to_string()])).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn lint_engine_reports_one_diagnostic_on_nonzero_exit() {
        // Drive the tool through an inline `sh -c` command rather than writing
        // and exec'ing a script file: exec'ing a freshly written executable can
        // transiently fail with ETXTBSY when a concurrent test thread forks
        // while this file's write fd is briefly open (CLOEXEC only closes on
        // exec, not fork). `sh -c` reaches the same stdout/stderr/exit-code
        // behaviour without ever exec'ing a file we just wrote.
        let tool = leak_tool(
            "fakelint",
            "sh",
            "linter",
            vec![
                "-c".to_string(),
                "echo 'problem on line 1' >&2\nexit 3".to_string(),
                PATH_PLACEHOLDER.to_string(),
            ],
        );
        let engine = lint_engine_default(tool, None, None).expect("non-mutating linter wires");
        assert!(engine.capabilities().lint);
        assert!(!engine.capabilities().format);

        let diagnostics = engine.lint(&make_src("file.txt", "anything\n"), &cfg()).unwrap();
        assert_eq!(diagnostics.len(), 1, "one file-level finding on failure");
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic.engine, "fakelint");
        assert_eq!(diagnostic.severity, Severity::Warning);
        assert!(diagnostic.span.is_none(), "no span at breadth-tier fidelity");
        assert!(diagnostic.code.is_none(), "no rule code");
        assert!(
            diagnostic.title.contains("problem on line 1"),
            "carries the tool's output: {}",
            diagnostic.title
        );
    }

    #[cfg(unix)]
    #[test]
    fn lint_engine_reports_nothing_on_zero_exit() {
        // Inline `sh -c` instead of exec'ing a freshly written script — see
        // `lint_engine_reports_one_diagnostic_on_nonzero_exit` for why (ETXTBSY
        // race under concurrent test threads).
        let tool = leak_tool(
            "oklint",
            "sh",
            "linter",
            vec!["-c".to_string(), "exit 0".to_string(), PATH_PLACEHOLDER.to_string()],
        );
        let engine = lint_engine_default(tool, None, None).unwrap();
        let diagnostics = engine.lint(&make_src("file.txt", "anything\n"), &cfg()).unwrap();
        assert!(diagnostics.is_empty(), "a passing run yields no diagnostics");
    }

    #[test]
    fn absent_binary_is_a_noop() {
        // A catalog tool whose binary is essentially never installed in CI must
        // degrade to Unchanged rather than erroring.
        let tool = Catalog::get()
            .tools()
            .iter()
            .find(|t| t.format_command().is_some() && probe_binary(&t.binary).is_none());
        if let Some(tool) = tool {
            let engine = format_engine_default(tool, None, None).unwrap();
            let result = engine.format(&make_src("file.txt", "anything\n"), &cfg()).unwrap();
            assert!(matches!(result, FormatOutput::Unchanged));
        }
    }

    #[cfg(unix)]
    #[test]
    fn env_var_is_visible_to_the_spawned_process() {
        // Prove the engine forwards `env` to the subprocess. Use `sh -c` inline
        // to avoid exec'ing a freshly written file (ETXTBSY race — see above).
        let tool = leak_tool(
            "envcheck",
            "sh",
            "linter",
            vec![
                "-c".to_string(),
                // Print the env var on stdout; exit non-zero so we can capture
                // it as a diagnostic message (exit 0 yields no diagnostics).
                "printf '%s' \"$POLY_TEST_VAR\"\nexit 1".to_string(),
                PATH_PLACEHOLDER.to_string(),
            ],
        );
        let env = BTreeMap::from([("POLY_TEST_VAR".to_string(), "hello-from-env".to_string())]);
        let engine = CatalogToolEngine::lint_engine(tool, None, None, env, None).expect("non-mutating linter wires");
        let diagnostics = engine.lint(&make_src("file.txt", "content\n"), &cfg()).unwrap();
        assert_eq!(diagnostics.len(), 1, "non-zero exit → one diagnostic");
        assert!(
            diagnostics[0].title.contains("hello-from-env"),
            "env var reflected in tool output: {}",
            diagnostics[0].title
        );
    }

    #[cfg(unix)]
    #[test]
    fn root_sets_the_working_directory_of_the_spawned_process() {
        // Prove the engine sets the working directory via `root`. The tool
        // prints the cwd; we canonicalize the expected path (macOS symlinks
        // /var/folders → /private/var/folders) before comparing.
        let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let tool = leak_tool(
            "cwdcheck",
            "sh",
            "linter",
            vec![
                "-c".to_string(),
                // Print cwd (via `pwd -P` for the physical, symlink-resolved
                // path) then exit non-zero so it surfaces as a diagnostic.
                "pwd -P\nexit 1".to_string(),
                PATH_PLACEHOLDER.to_string(),
            ],
        );
        let engine = CatalogToolEngine::lint_engine(tool, None, None, BTreeMap::new(), Some(tmp.clone()))
            .expect("non-mutating linter wires");
        let diagnostics = engine.lint(&make_src("file.txt", "content\n"), &cfg()).unwrap();
        assert_eq!(diagnostics.len(), 1, "non-zero exit → one diagnostic");
        let tmp_str = tmp.to_string_lossy();
        assert!(
            diagnostics[0].title.contains(tmp_str.as_ref()),
            "cwd reflects root override: {}",
            diagnostics[0].title
        );
    }

    /// Build a leaked `&'static Tool` with path_globs, for testing the path filter.
    #[cfg(unix)]
    fn leak_tool_with_globs(
        name: &str,
        binary: &str,
        category: &str,
        arguments: Vec<String>,
        path_globs: Vec<String>,
    ) -> &'static Tool {
        Box::leak(Box::new(Tool {
            name: name.to_string(),
            binary: binary.to_string(),
            categories: vec![category.to_string()],
            languages: vec!["yaml".to_string()],
            commands: BTreeMap::from([(
                String::new(),
                CatalogCommand {
                    arguments,
                    stdin: false,
                },
            )]),
            homepage: String::new(),
            path_globs,
        }))
    }

    /// A tool with `path_globs` must skip files that don't match and process
    /// files that do match. The tool always exits non-zero so we can distinguish
    /// "processed (diagnostic)" from "skipped (empty)".
    #[cfg(unix)]
    #[test]
    fn path_globs_skips_non_matching_and_runs_matching_files() {
        let tool = leak_tool_with_globs(
            "scopedlint",
            "sh",
            "linter",
            vec![
                "-c".to_string(),
                // Always fail, so a non-skipped file always produces a diagnostic.
                "exit 1".to_string(),
                PATH_PLACEHOLDER.to_string(),
            ],
            vec!["**/.github/workflows/**/*.yml".to_string()],
        );
        let engine = lint_engine_default(tool, None, None).expect("non-mutating linter wires");

        // Non-matching path → skipped (no diagnostics even though tool would fail).
        let non_match = engine.lint(&make_src("Taskfile.yml", ""), &cfg()).unwrap();
        assert!(
            non_match.is_empty(),
            "Taskfile.yml does not match .github/workflows/**/*.yml — must be skipped; got: {non_match:?}"
        );

        // Matching path → tool runs → diagnostic (exit 1).
        let matches = engine.lint(&make_src(".github/workflows/ci.yml", ""), &cfg()).unwrap();
        assert!(
            !matches.is_empty(),
            ".github/workflows/ci.yml matches the glob — tool must run and report; got: {matches:?}"
        );
    }

    #[test]
    fn actionlint_catalog_entry_has_github_workflows_path_globs() {
        let catalog = poly_catalog::Catalog::get();
        let tool = catalog.tool("actionlint").expect("actionlint is in the catalog");
        assert!(
            !tool.path_globs.is_empty(),
            "actionlint must declare path_globs to restrict it to workflow files"
        );
        assert!(
            tool.path_globs.iter().any(|g| g.contains(".github/workflows")),
            "actionlint path_globs must reference .github/workflows; got: {:?}",
            tool.path_globs
        );
    }
}
