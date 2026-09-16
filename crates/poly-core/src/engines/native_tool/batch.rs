//! Batch formatting for native-toolchain backends (ADR 0033).
//!
//! The per-file path spends most of its time not formatting. Measured on the
//! development host, `shfmt` costs ~18 ms per file of which ~17 ms is the Go
//! runtime starting up, and the JVM formatters are an order of magnitude worse
//! again. This module amortizes one spawn across many files.
//!
//! # How
//!
//! poly writes each file's **in-memory content** into a scratch mirror it owns,
//! runs the tool over the mirrored paths so it rewrites them in place, and reads
//! the results back. The real worktree is never handed to the tool: the runner's
//! atomic write remains the only thing that touches it, so `--check` stays a true
//! dry run and the convergence loop is unaffected.
//!
//! Passing content rather than paths is what makes this safe under the commit
//! gate (ADR 0019). poly already read the staged bytes; the tool sees exactly
//! those, cannot re-resolve a path into the worktree, and cannot follow a
//! `snapshot_include` symlink back out of the snapshot.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;

use super::format::normalize_newlines;
use super::spec::ToolSpec;
use crate::config::EngineConfig;
use crate::engine::{FormatOutput, SourceFile};

/// Conservative budget for the paths appended to one invocation's argv.
///
/// Windows is the binding constraint: `CreateProcess` caps the **entire**
/// command line at 32,767 characters, and `x86_64-pc-windows-msvc` is a release
/// target. 30,000 leaves room for the binary and its fixed flags. Unix limits
/// are far larger (1 MiB on the development host), so one number serves both.
///
/// Exceeding it splits into another invocation rather than truncating: a
/// truncated shard would silently drop files from the run while still reporting
/// the ones it kept as checked.
const ARGV_BUDGET: usize = 30_000;

/// Whether `[fmt.<lang>.<tool>] batch = true` asked for batch formatting.
///
/// **Off unless asked**, unlike `enabled`, which has a per-tool default.
/// Batching does not change what a formatter produces — output is
/// byte-identical — it changes how the work is bought: one process spawn in
/// exchange for writing a scratch mirror, having the tool read and rewrite it,
/// and reading it back. Which side is cheaper is environment-dependent.
/// Measured on one machine, `gofmt` ran 3x faster batched while `shfmt` —
/// near-identical startup cost — ran 4x slower; under heavy load both were
/// faster batched. There is no defensible universal default, so there isn't one.
///
/// This read is sited here rather than in the shared `native_tool/mod.rs` for
/// the same reason `use_tabs` lives in `tabs.rs`: the config-key audit
/// attributes a read to every engine whose sources include the file, and siting
/// it in `mod.rs` would force `batch` to be declared on the four tools that
/// cannot batch at all.
pub(crate) fn wants_batch(cfg: &EngineConfig) -> bool {
    cfg.options.get("batch").and_then(toml::Value::as_bool).unwrap_or(false)
}

/// Format every member of `batch` by mirroring it into a scratch tree.
///
/// Returns one entry per input, positionally aligned. The outer `Err` means the
/// batch as a whole could not be trusted — the caller re-runs these files
/// through the per-file path, so it costs time and never correctness.
pub(crate) fn format_batch_via_tool(
    spec: &ToolSpec,
    batch: &[SourceFile],
    indent_width: usize,
    use_tabs: bool,
) -> anyhow::Result<Vec<anyhow::Result<FormatOutput>>> {
    let format_binary = spec
        .format_binary
        .context("format_batch_via_tool called on a lint-only ToolSpec")?;
    let batch_args = spec
        .batch_format_args
        .context("format_batch_via_tool called on a tool that declares no batch arguments")?;
    if batch.is_empty() {
        return Ok(Vec::new());
    }

    // RAII: removed when this drops, including on panic or early return. The
    // `tempfile` crate creates it 0700 on Unix, which matters for the same
    // reason ADR 0019 hardens the staged snapshot — it holds a copy of the
    // repository's source.
    let scratch = tempfile::Builder::new()
        .prefix("poly-batch-")
        .tempdir()
        .context("creating scratch directory for batch formatting")?;

    let mirrored = materialize(scratch.path(), batch)?;

    // Every chunk must succeed. A non-zero exit is not a per-file verdict: a
    // tool rejecting one malformed member exits non-zero having correctly
    // formatted the rest, and a tool rejecting a *flag* exits non-zero having
    // done nothing at all — and from outside, after the fact, those are
    // indistinguishable. Reading files back in the second case would report
    // every one of them "unchanged", i.e. clean, which is a false pass. So any
    // non-zero exit discards the whole batch and falls back per-file, where each
    // file's own outcome is established individually.
    for chunk in chunks_within_budget(&mirrored) {
        let mut cmd = Command::new(format_binary);
        if spec.format_indent_flag {
            cmd.arg("-i");
            cmd.arg(if use_tabs {
                "0".to_owned()
            } else {
                indent_width.to_string()
            });
        }
        cmd.args(batch_args);
        cmd.args(chunk);
        let output = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .with_context(|| format!("failed to run '{format_binary}' over a batch"))?;
        if !output.status.success() {
            anyhow::bail!(
                "'{format_binary}' exited {} over a batch of {} file(s); falling back to per-file formatting",
                output
                    .status
                    .code()
                    .map_or_else(|| "by signal".to_owned(), |c| c.to_string()),
                chunk.len()
            );
        }
    }

    Ok(read_back(&mirrored, batch))
}

/// Write each member's content into `root`, one file per numbered subdirectory.
///
/// The **basename is preserved** because it is load-bearing: `shfmt` picks its
/// shell dialect from the filename, and `google-java-format` expects a Java
/// file to be named after its class. The numbered parent is what keeps two
/// files with the same basename from colliding, without reproducing (or
/// escaping from) the repository's own directory layout.
fn materialize(root: &Path, batch: &[SourceFile]) -> anyhow::Result<Vec<PathBuf>> {
    let mut mirrored = Vec::with_capacity(batch.len());
    for (index, src) in batch.iter().enumerate() {
        let dir = root.join(index.to_string());
        std::fs::create_dir(&dir).with_context(|| format!("creating batch scratch directory {}", dir.display()))?;
        let name = src.path.file_name().unwrap_or_else(|| std::ffi::OsStr::new("input"));
        let path = dir.join(name);
        std::fs::write(&path, src.content.as_bytes())
            .with_context(|| format!("writing batch input for {}", src.path.display()))?;
        mirrored.push(path);
    }
    Ok(mirrored)
}

/// Split `paths` into runs whose combined argv length stays under
/// [`ARGV_BUDGET`]. A single path longer than the budget still gets its own
/// invocation — splitting further is impossible, and dropping it would be a
/// silent coverage loss.
fn chunks_within_budget(paths: &[PathBuf]) -> Vec<&[PathBuf]> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut width = 0;
    for (index, path) in paths.iter().enumerate() {
        // +1 for the separator the OS accounts for between arguments.
        let cost = path.as_os_str().len() + 1;
        if index > start && width + cost > ARGV_BUDGET {
            chunks.push(&paths[start..index]);
            start = index;
            width = 0;
        }
        width += cost;
    }
    if start < paths.len() {
        chunks.push(&paths[start..]);
    }
    chunks
}

/// Read each mirrored file back and diff it against the content poly supplied.
///
/// A file that reads back byte-identical is [`FormatOutput::Unchanged`], which
/// is the same answer the per-file path gives for already-formatted input. This
/// is only sound because the caller has already established that the tool exited
/// zero over every chunk.
fn read_back(mirrored: &[PathBuf], batch: &[SourceFile]) -> Vec<anyhow::Result<FormatOutput>> {
    mirrored
        .iter()
        .zip(batch)
        .map(|(path, src)| {
            let formatted = std::fs::read_to_string(path)
                .map(normalize_newlines)
                .with_context(|| format!("reading batch output back for {}", src.path.display()))?;
            if formatted == *src.content {
                Ok(FormatOutput::Unchanged)
            } else {
                Ok(FormatOutput::Formatted(formatted))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::super::spec::SHFMT_SPEC;
    use super::{ARGV_BUDGET, chunks_within_budget, format_batch_via_tool};
    use crate::engine::{FormatOutput, SourceFile};
    use crate::language::Language;

    fn shell(path: &str, content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            language: Language::Shell,
            content: Arc::from(content),
        }
    }

    fn shfmt_present() -> bool {
        std::process::Command::new("shfmt")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Splitting must never drop a path: a truncated shard would report the
    /// files it kept as checked while silently omitting the rest.
    #[test]
    fn chunking_preserves_every_path() {
        let paths: Vec<PathBuf> = (0..5_000)
            .map(|i| PathBuf::from(format!("/tmp/poly/{i}/file.sh")))
            .collect();
        let chunks = chunks_within_budget(&paths);
        assert!(chunks.len() > 1, "5000 paths must exceed one argv budget");
        assert_eq!(chunks.iter().map(|c| c.len()).sum::<usize>(), paths.len());
        let flattened: Vec<&PathBuf> = chunks.iter().flat_map(|c| c.iter()).collect();
        assert_eq!(
            flattened,
            paths.iter().collect::<Vec<_>>(),
            "order and membership must survive chunking"
        );
        for chunk in &chunks {
            let width: usize = chunk.iter().map(|p| p.as_os_str().len() + 1).sum();
            assert!(
                width <= ARGV_BUDGET,
                "chunk of {width} bytes exceeds the {ARGV_BUDGET} budget"
            );
        }
    }

    /// A path longer than the whole budget still has to be run, not dropped.
    #[test]
    fn an_oversized_path_still_gets_its_own_chunk() {
        let long = PathBuf::from(format!("/tmp/{}", "x".repeat(ARGV_BUDGET + 10)));
        let paths = vec![PathBuf::from("/tmp/a.sh"), long.clone()];
        let chunks = chunks_within_budget(&paths);
        assert_eq!(chunks.iter().map(|c| c.len()).sum::<usize>(), 2);
        assert!(chunks.iter().flat_map(|c| c.iter()).any(|p| *p == long));
    }

    #[test]
    fn empty_batch_is_not_a_spawn() {
        let out = format_batch_via_tool(&SHFMT_SPEC, &[], 2, false).expect("an empty batch is trivially fine");
        assert!(out.is_empty());
    }

    /// The whole point: many files, one spawn, each answer attributed to its
    /// own input. The mixed clean/dirty membership is deliberate — a batch that
    /// only ever contained dirty files would not catch an implementation that
    /// returned `Formatted` for everything.
    #[test]
    fn a_batch_formats_each_member_and_attributes_it_correctly() {
        if !shfmt_present() {
            eprintln!("skipping: shfmt not on PATH");
            return;
        }
        let batch = vec![
            shell(
                "/repo/dirty1.sh",
                "if true; then
 echo a
fi
",
            ),
            shell(
                "/repo/clean.sh",
                "if true; then
  echo b
fi
",
            ),
            shell(
                "/repo/dirty2.sh",
                "if true; then
    echo c
fi
",
            ),
        ];
        let out = format_batch_via_tool(&SHFMT_SPEC, &batch, 2, false).expect("batch of valid shell must succeed");
        assert_eq!(out.len(), batch.len(), "the contract is one answer per input");

        match out[0].as_ref().expect("dirty1 is valid shell") {
            FormatOutput::Formatted(s) => assert_eq!(
                s,
                "if true; then
  echo a
fi
"
            ),
            FormatOutput::Unchanged => panic!("one-space indent must be reformatted to two"),
        }
        assert!(
            matches!(out[1].as_ref().expect("clean is valid shell"), FormatOutput::Unchanged),
            "already-formatted input must report Unchanged, not echo itself back"
        );
        match out[2].as_ref().expect("dirty2 is valid shell") {
            FormatOutput::Formatted(s) => assert_eq!(
                s,
                "if true; then
  echo c
fi
"
            ),
            FormatOutput::Unchanged => panic!("four-space indent must be reformatted to two"),
        }
    }

    /// The false-pass guard, and the reason the exit code is trusted over the
    /// file contents. A tool that fails *before doing anything* leaves every
    /// mirrored file byte-identical, which is indistinguishable from "all of
    /// them were already clean" — so a non-zero exit must fail the batch
    /// outright rather than report a verdict per file.
    #[test]
    fn a_malformed_member_fails_the_whole_batch_rather_than_reporting_clean() {
        if !shfmt_present() {
            eprintln!("skipping: shfmt not on PATH");
            return;
        }
        let batch = vec![
            shell(
                "/repo/good.sh",
                "if true; then
 echo a
fi
",
            ),
            shell(
                "/repo/broken.sh",
                "if true; then
 echo oops
",
            ),
        ];
        let error = format_batch_via_tool(&SHFMT_SPEC, &batch, 2, false)
            .expect_err("a non-zero exit must fail the batch, so the caller falls back per-file");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("falling back to per-file"),
            "the error must say the batch was discarded, not that the files are clean: {rendered}"
        );
    }

    /// Capability is about the tool's CLI, not about whether batching is a
    /// good idea for it. The two tools excluded here are excluded for
    /// correctness, not performance, so no amount of measurement re-enables
    /// them.
    #[test]
    fn only_tools_that_can_rewrite_named_files_declare_batch_capability() {
        use crate::config::{EngineConfig, GlobalDefaults};
        use crate::engine::Engine;
        use crate::engines::native_tool::NativeToolEngine;

        // Opted in, so what varies below is capability alone.
        let mut options = toml::Table::new();
        options.insert("enabled".to_owned(), toml::Value::Boolean(true));
        options.insert("batch".to_owned(), toml::Value::Boolean(true));
        let opted_in = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options,
        };

        for (engine, what) in [
            (NativeToolEngine::shell_format(), "shfmt -w"),
            (NativeToolEngine::for_language(Language::Go), "gofmt -w"),
            (NativeToolEngine::for_language(Language::Kotlin), "ktfmt"),
            (NativeToolEngine::for_language(Language::Java), "google-java-format -i"),
        ] {
            assert!(
                engine.batch_support(&opted_in).format,
                "{what} rewrites named files in place"
            );
        }

        assert!(
            !NativeToolEngine::for_language(Language::Rust)
                .batch_support(&opted_in)
                .format,
            "rustfmt discovers rustfmt.toml by walking up from the file; a scratch mirror does not \
             reproduce the repository's ancestors, so batching would change which config governs the run"
        );
        assert!(
            !NativeToolEngine::for_language(Language::Swift)
                .batch_support(&opted_in)
                .format,
            "swift-format reads .swift-format from the source tree; same reason as rustfmt"
        );
        assert!(
            !NativeToolEngine::for_language(Language::R)
                .batch_support(&opted_in)
                .format,
            "styler's argv is a poly-authored `Rscript -e` program that takes one path"
        );
    }

    /// Batching must be off unless explicitly asked for. A performance trade
    /// whose sign flips with the corpus and the machine load is not something
    /// poly can default for anyone.
    #[test]
    fn batching_is_off_unless_the_config_asks_for_it() {
        use crate::config::{EngineConfig, GlobalDefaults};
        use crate::engine::Engine;
        use crate::engines::native_tool::NativeToolEngine;

        let cfg = |value: Option<toml::Value>| {
            let mut options = toml::Table::new();
            options.insert("enabled".to_owned(), toml::Value::Boolean(true));
            if let Some(value) = value {
                options.insert("batch".to_owned(), value);
            }
            EngineConfig {
                globals: GlobalDefaults::default(),
                indent_width: 2,
                options,
            }
        };
        let shfmt = NativeToolEngine::shell_format();

        assert!(!shfmt.batch_support(&cfg(None)).format, "absent key must mean off");
        assert!(
            !shfmt.batch_support(&cfg(Some(toml::Value::Boolean(false)))).format,
            "false must mean off"
        );
        assert!(
            shfmt.batch_support(&cfg(Some(toml::Value::Boolean(true)))).format,
            "true must mean on"
        );
        // A non-bool is a config error the `polyconfig` lint reports; it must
        // not be read as consent here.
        assert!(
            !shfmt
                .batch_support(&cfg(Some(toml::Value::String("true".to_owned()))))
                .format,
            "a string must not switch batching on"
        );
        // Opting in cannot override a capability exclusion.
        assert!(
            !NativeToolEngine::for_language(Language::Rust)
                .batch_support(&cfg(Some(toml::Value::Boolean(true))))
                .format,
            "rustfmt cannot batch regardless of config: a scratch mirror does not reproduce its config ancestry"
        );
    }

    /// `shfmt` selects its shell dialect from the filename, so a mirror that
    /// renamed inputs would silently change how they are parsed.
    #[test]
    fn the_basename_survives_into_the_mirror() {
        if !shfmt_present() {
            eprintln!("skipping: shfmt not on PATH");
            return;
        }
        // `function f() { ... }` is valid bash and rejected by POSIX sh. If the
        // mirror dropped the `.bash` name, shfmt would fall back to its default
        // dialect and this would not round-trip.
        let batch = vec![shell(
            "/repo/script.bash",
            "function f() {
 echo hi
}
",
        )];
        let out = format_batch_via_tool(&SHFMT_SPEC, &batch, 2, false).expect("valid bash must batch cleanly");
        match out[0].as_ref().expect("valid bash") {
            FormatOutput::Formatted(s) => assert!(s.contains("echo hi"), "content must survive: {s}"),
            FormatOutput::Unchanged => panic!("one-space indent must be reformatted"),
        }
    }
}
