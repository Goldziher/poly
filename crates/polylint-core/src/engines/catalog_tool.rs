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
//! user has explicitly enabled, and routed by [`crate::registry`] (hence
//! [`Engine::languages`] returns an empty slice).
//!
//! This is **format-only**; catalog linting is a separate, later tier.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use anyhow::Context;
use poly_catalog::{PATH_PLACEHOLDER, Tool};

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Engine, FormatOutput, SourceFile};
use crate::language::Language;

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
    if let Some(cached) = probe_cache()
        .lock()
        .expect("probe cache poisoned")
        .get(binary)
    {
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

/// A catalog formatter wired as an [`Engine`] for one enabled `[tools.<name>]`.
pub struct CatalogToolEngine {
    tool: &'static Tool,
    /// Resolved argv (catalog command's arguments, or the user's `args` override).
    arguments: Vec<String>,
    /// Whether the tool reads source on stdin (vs. a `$PATH` file).
    stdin: bool,
    /// Cache-key version: folds the probed binary version, the resolved argv, and
    /// the stdin mode so any change invalidates stale cached results.
    version: String,
}

impl CatalogToolEngine {
    /// Build a formatter engine for `tool`. `command_name` selects the catalog
    /// command (`None` → the tool's [`Tool::format_command`]); `args_override`
    /// replaces the command's argv when present. Returns `None` when the tool
    /// exposes no usable format command.
    pub fn format_engine(
        tool: &'static Tool,
        command_name: Option<&str>,
        args_override: Option<&[String]>,
    ) -> Option<Self> {
        let command = match command_name {
            Some(name) => tool.command(name)?,
            None => tool.format_command()?.1,
        };
        let arguments = args_override
            .map(<[String]>::to_vec)
            .unwrap_or_else(|| command.arguments.clone());
        let stdin = command.stdin;
        let probe = probe_binary(&tool.binary);
        let version = format!(
            "catalog:{}:{}:stdin={stdin}:args={arguments:?}",
            tool.name,
            probe.as_deref().unwrap_or("absent"),
        );
        Some(CatalogToolEngine {
            tool,
            arguments,
            stdin,
            version,
        })
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
            lint: false,
            format: true,
            fix: false,
        }
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn format(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
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
}

impl CatalogToolEngine {
    /// stdin → stdout formatting.
    fn format_via_stdin(&self, src: &SourceFile) -> anyhow::Result<FormatOutput> {
        let binary = &self.tool.binary;
        let argv = self.argv_with_path("-");
        let mut child = Command::new(binary)
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
            WriteOutcome::Thread(thread::spawn(move || {
                stdin_handle.write_all(content.as_bytes())
            }))
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

        let formatted = String::from_utf8(output.stdout)
            .with_context(|| format!("'{binary}' produced non-UTF-8 output"))?;
        Ok(diff_output(formatted, src))
    }

    /// Temp-file (`$PATH`) formatting: write source to a temp file, run the tool
    /// (which rewrites it in place), and read it back.
    fn format_via_path(&self, src: &SourceFile) -> anyhow::Result<FormatOutput> {
        let binary = &self.tool.binary;
        let extension = src
            .path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("txt");
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
        let output = Command::new(binary)
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
    use std::path::PathBuf;

    use poly_catalog::Catalog;

    use super::*;
    use crate::config::{EngineConfig, GlobalDefaults};

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

    #[test]
    fn format_engine_builds_for_a_catalog_formatter() {
        let tool = Catalog::get().tool("shfmt").expect("shfmt in catalog");
        let engine = CatalogToolEngine::format_engine(tool, None, None)
            .expect("shfmt exposes a format command");
        assert_eq!(engine.name(), "shfmt");
        assert!(engine.capabilities().format);
        assert!(!engine.capabilities().lint);
        assert!(engine.version().contains("shfmt"));
    }

    #[test]
    fn format_engine_none_for_pure_linter() {
        // shellcheck is lint-only; it has no format command.
        if let Some(tool) = Catalog::get().tool("shellcheck") {
            assert!(CatalogToolEngine::format_engine(tool, None, None).is_none());
        }
    }

    #[test]
    fn args_override_replaces_catalog_argv() {
        let tool = Catalog::get().tool("shfmt").expect("shfmt in catalog");
        let engine =
            CatalogToolEngine::format_engine(tool, None, Some(&["--custom".to_string()])).unwrap();
        assert_eq!(engine.arguments, vec!["--custom".to_string()]);
        assert!(engine.version().contains("--custom"));
    }

    #[test]
    fn argv_substitutes_path_placeholder() {
        let tool = Catalog::get().tool("gofmt").expect("gofmt in catalog");
        let engine = CatalogToolEngine::format_engine(tool, None, None).unwrap();
        let argv = engine.argv_with_path("/tmp/x.go");
        assert!(argv.iter().any(|a| a == "/tmp/x.go"));
        assert!(!argv.iter().any(|a| a == PATH_PLACEHOLDER));
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
            let engine = CatalogToolEngine::format_engine(tool, None, None).unwrap();
            let result = engine
                .format(&make_src("file.txt", "anything\n"), &cfg())
                .unwrap();
            assert!(matches!(result, FormatOutput::Unchanged));
        }
    }
}
