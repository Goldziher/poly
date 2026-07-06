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

/// Catalog tools that are **whole-project type-checkers**, not per-file linters.
///
/// They resolve imports across the whole project — sibling modules, the project's
/// dependency graph, its virtualenv — and infer an import root from the project
/// layout. poly's catalog tier runs one process per file with an exit-code verdict,
/// which starves a type-checker of that context and turns every cross-module import
/// into a spurious `missing-import`. They are therefore **not wired** as catalog
/// linters ([`CatalogToolEngine::lint_engine`] returns `None`); a project that wants
/// them should run them as a dedicated whole-project step outside poly.
const WHOLE_PROJECT_LINTERS: &[&str] = &["pyrefly", "mypy", "ty", "pyright", "pyre", "pytype", "tsc"];

/// Whether `name` is a whole-project type-checker unsuited to the per-file catalog
/// lint tier (see [`WHOLE_PROJECT_LINTERS`]).
pub(crate) fn is_whole_project_linter(name: &str) -> bool {
    WHOLE_PROJECT_LINTERS.contains(&name)
}

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
    /// Returns `None` when the tool exposes no usable lint command, when the
    /// resolved argv is mutating (contains any of [`MUTATING_FLAGS`]) — a command
    /// that rewrites the file must never run as a linter, since the runner does
    /// not expect a lint pass to touch the file — or when the tool is a
    /// whole-project type-checker ([`WHOLE_PROJECT_LINTERS`]) that cannot run per
    /// file. Such a tool is simply not registered as a linter (it can still be
    /// wired as a formatter).
    pub fn lint_engine(
        tool: &'static Tool,
        command_name: Option<&str>,
        args_override: Option<&[String]>,
        env: BTreeMap<String, String>,
        root: Option<PathBuf>,
    ) -> Option<Self> {
        // Whole-project type-checkers cannot work per file (see
        // `WHOLE_PROJECT_LINTERS`): never wire them as catalog linters.
        if is_whole_project_linter(&tool.name) {
            return None;
        }
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

    /// Lint via a file path (`$PATH`).
    ///
    /// Linting is read-only, so it runs against the **real on-disk file** whenever
    /// that file exists and still matches the in-memory content. This preserves the
    /// project context a temp copy destroys: a Python tool resolves sibling modules
    /// and the project virtualenv, and a path-sensitive tool (e.g. actionlint's
    /// `.github/workflows/` detection) sees the true path. Only when the real file
    /// is unavailable or has diverged from the content being linted (e.g. a re-lint
    /// after an in-memory fix, or synthetic content with no backing file) does it
    /// fall back to an isolated temp copy the tool merely reads.
    fn lint_via_path(&self, src: &SourceFile) -> anyhow::Result<LintOutcome> {
        if let Some(real) = real_path_if_matches(&src.path, &src.content) {
            return self.run_lint_on_path(&real.to_string_lossy());
        }
        let extension = src.path.extension().and_then(|ext| ext.to_str()).unwrap_or("txt");
        let mut temp = tempfile::Builder::new()
            .prefix("poly-catalog-")
            .suffix(&format!(".{extension}"))
            .tempfile()
            .context("creating temp file for catalog tool")?;
        temp.write_all(src.content.as_bytes())
            .context("writing source to temp file")?;
        temp.flush().context("flushing temp file")?;
        self.run_lint_on_path(&temp.path().to_string_lossy())
    }

    /// Run the path-based lint command against `path`, capturing its exit status
    /// and combined output. Shared by the real-file and temp-copy variants of
    /// [`CatalogToolEngine::lint_via_path`].
    fn run_lint_on_path(&self, path: &str) -> anyhow::Result<LintOutcome> {
        let binary = &self.tool.binary;
        let argv = self.argv_with_path(path);
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

/// The canonical, absolute path of `path` when it exists on disk with content
/// byte-identical to `content`; `None` otherwise.
///
/// Used to decide whether a read-only linter can run against the real file
/// (preserving project context) instead of an isolated temp copy. The path is
/// canonicalised to an absolute one so the tool resolves it regardless of any
/// `root` working-directory override.
fn real_path_if_matches(path: &std::path::Path, content: &str) -> Option<PathBuf> {
    let bytes = std::fs::read(path).ok()?;
    if bytes != content.as_bytes() {
        return None;
    }
    std::fs::canonicalize(path).ok()
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
mod tests;
