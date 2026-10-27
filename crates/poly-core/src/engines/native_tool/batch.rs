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

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;

use super::format::normalize_newlines;
use super::isolate::{Isolation, Members};
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

/// Whether batching is wanted: on unless `batch = false` opts out, in
/// `[fmt.<lang>.<tool>]` for the formatters and `[lint.<lang>.shellcheck]` for
/// the one linter.
///
/// Batching does not change what a tool produces — output is byte-identical,
/// asserted against the stdin path on real corpora — it changes how the work is
/// bought: a handful of process spawns in exchange for writing a scratch mirror
/// and reading it back. It shipped opt-in because a first measurement found it
/// four times slower, but that measurement was of a version that discarded a
/// whole shard when any member failed to parse, so one bad script silently sent
/// every file back to a spawn each *and* paid for the mirror. With failures
/// isolated to the members that cause them (see [`super::isolate`]) the CPU cost
/// of the two paths is equal, and the wall-clock difference is the spawn
/// latency, which is large exactly where it hurts: a loaded machine, or one
/// whose endpoint security scans every `exec`. 477 scripts took 41 s to lint
/// per file and 9 s batched on such a machine; on an idle one both are small.
///
/// This read is sited here rather than in the shared `native_tool/mod.rs` for
/// the same reason `use_tabs` lives in `tabs.rs`: the config-key audit
/// attributes a read to every engine whose sources include the file, and siting
/// it in `mod.rs` would force `batch` to be declared on the four tools that
/// cannot batch at all.
pub(crate) fn wants_batch(cfg: &EngineConfig) -> bool {
    cfg.options.get("batch").and_then(toml::Value::as_bool).unwrap_or(true)
}

/// Format every member of `batch` by mirroring it into a scratch tree.
///
/// Returns one entry per input, positionally aligned. A member the tool refused
/// is an `Err` in its own slot, which the caller answers by running that file
/// through the per-file path. The outer `Err` means the batch as a whole could
/// not be trusted, and the caller re-runs every file that way — so either shape
/// costs time and never correctness.
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

    let mut run = Run {
        binary: format_binary,
        fixed_args: {
            let mut args = Vec::new();
            if spec.format_indent_flag {
                args.push("-i".to_owned());
                args.push(if use_tabs {
                    "0".to_owned()
                } else {
                    indent_width.to_string()
                });
            }
            args.extend(batch_args.iter().map(|arg| (*arg).to_owned()));
            args
        },
        batch,
        mirrored: materialize(scratch.path(), batch, None)?,
        outcomes: batch
            .iter()
            .map(|_| Err(anyhow::anyhow!("not formatted in a batch")))
            .collect(),
    };
    let mut isolation = Isolation::default();
    for range in chunks_within_budget(&run.mirrored) {
        isolation.run(&mut run, range)?;
    }
    Ok(run.outcomes)
}

/// One batch in flight: the mirror, the tool to run over it, and what has been
/// established about each member so far.
struct Run<'a> {
    binary: &'a str,
    fixed_args: Vec<String>,
    batch: &'a [SourceFile],
    mirrored: Vec<PathBuf>,
    outcomes: Vec<anyhow::Result<FormatOutput>>,
}

impl Members for Run<'_> {
    fn attempt(&mut self, range: &Range<usize>) -> anyhow::Result<bool> {
        for index in range.clone() {
            std::fs::write(&self.mirrored[index], self.batch[index].content.as_bytes())
                .with_context(|| format!("writing batch input for {}", self.batch[index].path.display()))?;
        }
        let output = Command::new(self.binary)
            .args(&self.fixed_args)
            .args(&self.mirrored[range.clone()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .with_context(|| format!("failed to run '{}' over a batch", self.binary))?;
        Ok(output.status.success())
    }

    fn accept(&mut self, range: &Range<usize>) {
        for index in range.clone() {
            self.outcomes[index] = read_member(&self.mirrored[index], &self.batch[index]);
        }
    }

    fn refuse(&mut self, index: usize) {
        self.outcomes[index] = Err(anyhow::anyhow!(
            "'{}' refused {} in a batch; formatting it on its own",
            self.binary,
            self.batch[index].path.display()
        ));
    }

    fn tool(&self) -> &str {
        self.binary
    }
}

/// Lay out one numbered subdirectory per member under `root` and return the path
/// each member's content will be written to.
///
/// The **basename is preserved** because it is load-bearing: `shfmt` picks its
/// shell dialect from the filename, and `google-java-format` expects a Java
/// file to be named after its class. The numbered parent is what keeps two
/// files with the same basename from colliding, without reproducing (or
/// escaping from) the repository's own directory layout.
///
/// `fixed_name` replaces the basename for a tool, like `shellcheck`, whose
/// answer changes with a name it would not have been given on stdin.
pub(super) fn materialize(root: &Path, batch: &[SourceFile], fixed_name: Option<&str>) -> anyhow::Result<Vec<PathBuf>> {
    let mut mirrored = Vec::with_capacity(batch.len());
    for (index, src) in batch.iter().enumerate() {
        let dir = root.join(index.to_string());
        std::fs::create_dir(&dir).with_context(|| format!("creating batch scratch directory {}", dir.display()))?;
        let name = fixed_name
            .map(std::ffi::OsStr::new)
            .or_else(|| src.path.file_name())
            .unwrap_or_else(|| std::ffi::OsStr::new("input"));
        mirrored.push(dir.join(name));
    }
    Ok(mirrored)
}

/// Split `paths` into runs whose combined argv length stays under
/// [`ARGV_BUDGET`], as index ranges. A single path longer than the budget still
/// gets its own invocation — splitting further is impossible, and dropping it
/// would be a silent coverage loss.
pub(super) fn chunks_within_budget(paths: &[PathBuf]) -> Vec<Range<usize>> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut width = 0;
    for (index, path) in paths.iter().enumerate() {
        // +1 for the separator the OS accounts for between arguments.
        let cost = path.as_os_str().len() + 1;
        if index > start && width + cost > ARGV_BUDGET {
            chunks.push(start..index);
            start = index;
            width = 0;
        }
        width += cost;
    }
    if start < paths.len() {
        chunks.push(start..paths.len());
    }
    chunks
}

/// Read one mirrored file back and compare it with the content poly supplied.
///
/// A file that reads back byte-identical is [`FormatOutput::Unchanged`], which
/// is the same answer the per-file path gives for already-formatted input. This
/// is only sound because the caller has already established that the tool exited
/// zero over the invocation that included it.
fn read_member(path: &Path, src: &SourceFile) -> anyhow::Result<FormatOutput> {
    let formatted = std::fs::read_to_string(path)
        .map(normalize_newlines)
        .with_context(|| format!("reading batch output back for {}", src.path.display()))?;
    if formatted == *src.content {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(formatted))
    }
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
        assert_eq!(chunks.iter().map(ExactSizeIterator::len).sum::<usize>(), paths.len());
        let flattened: Vec<&PathBuf> = chunks.iter().flat_map(|c| paths[c.clone()].iter()).collect();
        assert_eq!(
            flattened,
            paths.iter().collect::<Vec<_>>(),
            "order and membership must survive chunking"
        );
        for chunk in &chunks {
            let width: usize = paths[chunk.clone()].iter().map(|p| p.as_os_str().len() + 1).sum();
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
        assert_eq!(chunks.iter().map(ExactSizeIterator::len).sum::<usize>(), 2);
        assert!(chunks.iter().flat_map(|c| paths[c.clone()].iter()).any(|p| *p == long));
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
    /// file contents: a failing invocation is never read back, because a tool
    /// that fails *before doing anything* leaves every mirrored file
    /// byte-identical, which is indistinguishable from "all of them were already
    /// clean". The malformed member is therefore refused individually, and the
    /// good one beside it is still formatted.
    #[test]
    fn a_malformed_member_is_refused_alone_and_never_reported_clean() {
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
        let out =
            format_batch_via_tool(&SHFMT_SPEC, &batch, 2, false).expect("one bad member must not discard the batch");
        assert_eq!(out.len(), 2);
        match out[0].as_ref().expect("the good member is answered") {
            FormatOutput::Formatted(s) => assert_eq!(s, "if true; then\n  echo a\nfi\n"),
            FormatOutput::Unchanged => panic!("one-space indent must be reformatted to two"),
        }
        let refusal = out[1]
            .as_ref()
            .expect_err("the malformed member must not be reported as clean");
        assert!(
            format!("{refusal:#}").contains("broken.sh"),
            "the refusal must name the file so the per-file path can report it: {refusal:#}"
        );
    }

    /// A stand-in formatter that exits 1 when any named file starts with `BAD`,
    /// rewrites the ones it accepts to `OK`, and logs one line per invocation —
    /// which makes the number of spawns observable without depending on a real
    /// tool. Shell builtins only, so a run costs one process however many files
    /// it names.
    #[cfg(unix)]
    fn fake_formatter(directory: &std::path::Path) -> (&'static str, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;

        let log = directory.join("invocations.log");
        let script = directory.join("fakefmt");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho run >> '{log}'\nstatus=0\nfor f in \"$@\"; do\n  case \"$f\" in -*) continue;; esac\n  \
                 IFS= read -r first < \"$f\"\n  if [ \"$first\" = BAD ]; then status=1; else printf 'OK\\n' > \"$f\"; fi\ndone\nexit $status\n",
                log = log.display()
            ),
        )
        .expect("write fake formatter");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
        let leaked: &'static str = Box::leak(script.display().to_string().into_boxed_str());
        (leaked, log)
    }

    #[cfg(unix)]
    fn fake_spec(binary: &'static str, batch_args: &'static [&'static str]) -> super::super::spec::ToolSpec {
        super::super::spec::ToolSpec {
            format_binary: Some(binary),
            batch_format_args: Some(batch_args),
            format_indent_flag: false,
            ..SHFMT_SPEC
        }
    }

    #[cfg(unix)]
    fn invocations(log: &std::path::Path) -> usize {
        std::fs::read_to_string(log).map_or(0, |text| text.lines().count())
    }

    /// Real repositories always contain a script the formatter cannot parse.
    /// Discarding the whole batch for it made batching save almost nothing; the
    /// failure must stay with the bad members and cost only a handful of extra
    /// spawns, not one per file.
    #[cfg(unix)]
    #[test]
    fn bad_members_cost_a_logarithmic_number_of_spawns_and_spoil_nothing_else() {
        let directory = tempfile::tempdir().expect("tempdir");
        let (binary, log) = fake_formatter(directory.path());
        let total = 256;
        let bad = [5, 200];
        let batch: Vec<SourceFile> = (0..total)
            .map(|i| {
                shell(
                    &format!("/repo/s{i}.sh"),
                    if bad.contains(&i) { "BAD\n" } else { "fine\n" },
                )
            })
            .collect();

        let out = format_batch_via_tool(&fake_spec(binary, &["-w"]), &batch, 2, false).expect("batch answered");

        for (i, outcome) in out.iter().enumerate() {
            if bad.contains(&i) {
                assert!(outcome.is_err(), "member {i} was refused and must fall back per-file");
            } else {
                match outcome.as_ref().expect("good members are answered") {
                    FormatOutput::Formatted(text) => assert_eq!(text, "OK\n", "member {i}"),
                    FormatOutput::Unchanged => panic!("member {i} was not formatted"),
                }
            }
        }
        let spawns = invocations(&log);
        assert!(
            spawns <= 2 * bad.len() * 9 + 2,
            "{spawns} invocations for {total} files with {} bad members is not logarithmic",
            bad.len()
        );
        assert!(
            spawns < total,
            "isolating the bad members must beat one spawn per file, got {spawns}"
        );
    }

    /// A tool that rejects the *invocation* refuses every member. That must be
    /// recognised after two refusals rather than halved into one spawn per file,
    /// and it must fail the batch, not report the untouched files clean.
    #[cfg(unix)]
    #[test]
    fn a_tool_that_refuses_everything_abandons_the_batch_quickly() {
        let directory = tempfile::tempdir().expect("tempdir");
        let (binary, log) = fake_formatter(directory.path());
        let batch: Vec<SourceFile> = (0..64).map(|i| shell(&format!("/repo/s{i}.sh"), "BAD\n")).collect();

        let error = format_batch_via_tool(&fake_spec(binary, &["-w"]), &batch, 2, false)
            .expect_err("a batch no member of which was accepted must fail as a whole");

        assert!(format!("{error:#}").contains("falling back to per-file"), "{error:#}");
        assert!(
            invocations(&log) <= 12,
            "gave up after {} invocations; it should stop once nothing has ever been accepted",
            invocations(&log)
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

    /// Batching is on unless the config opts out, and only a real `false` opts out.
    #[test]
    fn batching_is_on_unless_the_config_opts_out() {
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

        assert!(shfmt.batch_support(&cfg(None)).format, "absent key must mean on");
        assert!(
            !shfmt.batch_support(&cfg(Some(toml::Value::Boolean(false)))).format,
            "false must mean off"
        );
        assert!(
            shfmt.batch_support(&cfg(Some(toml::Value::Boolean(true)))).format,
            "true must mean on"
        );
        // A non-bool is a config error the `polyconfig` lint reports; it must
        // not be read as a request to turn batching off.
        assert!(
            shfmt
                .batch_support(&cfg(Some(toml::Value::String("false".to_owned()))))
                .format,
            "a string must not switch batching off"
        );
        // Lint batching rides on the same key, for shellcheck alone.
        let shellcheck = NativeToolEngine::shell_lint();
        assert!(shellcheck.batch_support(&cfg(None)).lint);
        assert!(!shellcheck.batch_support(&cfg(Some(toml::Value::Boolean(false)))).lint);
        assert!(
            !shfmt.batch_support(&cfg(None)).lint,
            "a formatter has nothing to lint in a batch"
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
