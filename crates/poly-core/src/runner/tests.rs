//! Tests for the pipeline runner.
//!
//! Split out because `runner.rs` sits at the repository's 1000-line module cap;
//! a path named `tests.rs` is exempt from it.

use super::*;

#[test]
fn cache_write_policy_skips_cheap_results() {
    assert!(!should_cache_result(
        MIN_CACHE_DURATION - std::time::Duration::from_nanos(1)
    ));
    assert!(should_cache_result(MIN_CACHE_DURATION));
}

#[test]
fn disabled_cache_skips_digest_work() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache = ResultCache::open(tmp.path().join("cache"), false).expect("open disabled cache");
    let digest_computed = std::cell::Cell::new(false);

    let digest = digest_if_enabled(&cache, || {
        digest_computed.set(true);
        ResultCache::single_file_digest("content")
    });

    assert!(digest.is_none());
    assert!(!digest_computed.get(), "disabled caching must not hash file contents");
}

/// A pass that is already at its fixed point runs exactly once — no wasted
/// confirmation pass, and the content is returned unchanged.
#[test]
fn format_to_fixed_point_stable_input_runs_once() {
    let calls = std::cell::Cell::new(0);
    let result = format_to_fixed_point(Arc::from("stable"), |content, _record| {
        calls.set(calls.get() + 1);
        Ok(Arc::clone(content))
    })
    .unwrap();
    assert_eq!(&*result.0, "stable");
    assert!(result.1, "an unchanged input is at its fixed point");
    assert_eq!(calls.get(), 1, "an already-stable input needs a single pass");
}

/// A non-idempotent pass (strips one trailing '!' per run) converges within
/// the bound, and the driver stops as soon as a pass makes no change — so the
/// result is a genuine fixed point, mirroring the `fmt --fix` then `--check`
/// invariant.
#[test]
fn format_to_fixed_point_converges_on_non_idempotent_pass() {
    let calls = std::cell::Cell::new(0);
    let result = format_to_fixed_point(Arc::from("a!!!"), |content, _record| {
        calls.set(calls.get() + 1);
        let stripped = content.strip_suffix('!').unwrap_or(content);
        Ok(Arc::from(stripped))
    })
    .unwrap();
    assert_eq!(&*result.0, "a", "trailing markers fully removed");
    assert!(result.1, "and it settled inside the bound");
    // "a!!!"->"a!!"->"a!"->"a" is 3 changing passes plus 1 no-op that proves
    // stability = 4 calls. ~keep
    assert_eq!(calls.get(), 4);
}

/// Only the first pass records debug so per-engine timing counts are not
/// inflated by the convergence retries.
#[test]
fn format_to_fixed_point_records_debug_on_first_pass_only() {
    let recorded: std::cell::RefCell<Vec<bool>> = std::cell::RefCell::new(Vec::new());
    format_to_fixed_point(Arc::from("a!!"), |content, record| {
        recorded.borrow_mut().push(record);
        Ok(Arc::from(content.strip_suffix('!').unwrap_or(content)))
    })
    .unwrap();
    assert_eq!(
        recorded.into_inner(),
        vec![true, false, false],
        "debug is recorded on the first pass, never on a retry"
    );
}

/// A backend that never stabilizes is bounded to `MAX_FORMAT_PASSES` runs
/// rather than looping forever — and **says** it did not settle.
///
/// Reporting that is the whole point. The hardening harness found real HTML
/// files where `poly fmt --fix` produced different bytes on seven successive
/// runs while reporting success each time; a following `poly fmt --check`
/// reports drift forever, and nothing in the output explained why.
#[test]
fn format_to_fixed_point_reports_a_never_stable_pass() {
    let calls = std::cell::Cell::new(0);
    let (content, settled) = format_to_fixed_point(Arc::from("x"), |content, _record| {
        calls.set(calls.get() + 1);
        Ok(Arc::from(format!("{content}y")))
    })
    .unwrap();
    assert_eq!(calls.get(), MAX_FORMAT_PASSES, "oscillation is capped, not infinite");
    assert_eq!(&*content, "xyyyyy", "returns the last bounded pass output");
    assert!(!settled, "and reports that the file is still changing");
}

/// Recurse `depth` frames, each pinning ~8 KiB of stack, returning the
/// accumulated depth. `black_box` keeps the per-frame buffer from being
/// optimised away, so the stack actually grows.
fn recurse_pinning_stack(depth: usize) -> usize {
    let mut frame = [0u8; 8 * 1024];
    frame[0] = (depth & 0xff) as u8;
    std::hint::black_box(&frame);
    if depth == 0 {
        frame[0] as usize
    } else {
        recurse_pinning_stack(depth - 1).wrapping_add(1)
    }
}

/// A worker thread sized at [`WORKER_STACK_SIZE`] must accommodate recursion
/// far deeper than the 2 MiB default rayon stack — the regression that made
/// per-file engines abort the whole run on nested real-world files
/// (spikard corpus). ~640 frames × 8 KiB ≈ 5 MiB of pinned stack overflows
/// the old 2 MiB default but fits comfortably in 16 MiB.
#[test]
fn worker_stack_accommodates_deep_recursion() {
    const FRAMES: usize = 640;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .stack_size(WORKER_STACK_SIZE)
        .build()
        .expect("build local pool");
    let result = pool.install(|| recurse_pinning_stack(FRAMES));
    assert_eq!(result, FRAMES);
}

mod panic_containment {
    //! The runner turns a panic inside an in-process parser into a per-file
    //! error instead of letting rayon carry it out of the worker and kill the run
    //! (see `engine_guard`).
    //!
    //! The containment used to be pinned by feeding a real parser an input that
    //! made it panic. That is not a stable fixture: the upstream bug in use was
    //! fixed by a dependency upgrade and the tests silently stopped testing
    //! anything. The engine here panics on demand, so they cannot.
    use super::*;
    use crate::config::GlobalDefaults;
    use crate::engine::{Capabilities, Engine, Severity};
    use crate::filter::SeverityRemap;

    const TRIGGER: &str = "PANIC-HERE";

    /// An engine that panics, the way a parser asserting on valid input does,
    /// for any source containing [`TRIGGER`] and otherwise behaves normally.
    struct PanickingParser;

    impl Engine for PanickingParser {
        fn name(&self) -> &'static str {
            "panicking-parser"
        }

        fn languages(&self) -> &'static [Language] {
            &[Language::Python]
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                lint: true,
                format: true,
                fix: false,
            }
        }

        fn version(&self) -> &str {
            "test"
        }

        fn lint(&self, src: &SourceFile, _cfg: &crate::config::EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
            assert!(!src.content.contains(TRIGGER), "parser invariant violated at offset 7");
            Ok(vec![finding()])
        }

        fn format(&self, src: &SourceFile, _cfg: &crate::config::EngineConfig) -> anyhow::Result<FormatOutput> {
            assert!(!src.content.contains(TRIGGER), "parser invariant violated at offset 7");
            Ok(FormatOutput::Formatted(src.content.to_uppercase()))
        }
    }

    fn finding() -> Diagnostic {
        Diagnostic {
            engine: "panicking-parser".to_owned(),
            code: Some("T001".to_owned()),
            severity: Severity::Warning,
            title: "a real finding".to_owned(),
            description: None,
            url: None,
            span: None,
            fix: Vec::new(),
            metadata: std::collections::BTreeMap::new(),
        }
    }

    fn plans() -> Vec<EnginePlan> {
        vec![EnginePlan {
            engine: Box::new(PanickingParser),
            config: crate::config::EngineConfig {
                globals: GlobalDefaults::default(),
                indent_width: 4,
                options: toml::Table::new(),
            },
            serialized_args: ResultCache::serialize_args(&toml::Table::new()),
            severity_remap: SeverityRemap::new(Vec::new()),
            provides_language_lint: true,
            format_ignore: None,
        }]
    }

    fn discovered(path: &std::path::Path) -> DiscoveredFile {
        DiscoveredFile {
            path: path.to_path_buf(),
            language: Language::Python,
            config_id: 0,
        }
    }

    fn disabled_cache(directory: &std::path::Path) -> ResultCache {
        ResultCache::open(directory.join("cache"), false).expect("open disabled cache")
    }

    #[test]
    fn a_lint_panic_becomes_an_error_naming_the_backend_and_the_file() {
        let directory = tempfile::tempdir().expect("tempdir");
        let bad = directory.path().join("bad.py");
        let error = lint_content(
            &discovered(&bad),
            &plans(),
            TRIGGER,
            false,
            &[],
            false,
            &LintPrefill::default(),
        )
        .expect_err("a panicking backend must surface as an error, not unwind");

        let message = format!("{error:#}");
        assert!(message.contains("panicked"), "{message}");
        assert!(message.contains("panicking-parser"), "{message}");
        assert!(
            message.contains("parser invariant violated"),
            "the upstream message is what makes the report actionable: {message}"
        );
    }

    #[test]
    fn a_format_panic_becomes_an_error_instead_of_killing_the_run() {
        let directory = tempfile::tempdir().expect("tempdir");
        let bad = directory.path().join("bad.py");
        std::fs::write(&bad, TRIGGER).expect("write fixture");
        let plan = RunPlan::serving(0, Language::Python, plans());

        let error = format_one(
            &discovered(&bad),
            &plan,
            &disabled_cache(directory.path()),
            false,
            false,
            false,
            &[true],
            &BatchPrefill::default(),
            (None, Resolution::Unavailable),
        )
        .expect_err("a panicking backend must surface as an error, not unwind");

        let message = format!("{error:#}");
        assert!(message.contains("panicked"), "{message}");
        assert!(message.contains("panicking-parser"), "{message}");
    }

    /// The failure that costs a user: everything else in the repository
    /// disappearing. Run through rayon, which is what propagates a worker panic
    /// to the caller — and asserted on a neighbour with a real finding, so a run
    /// that returned nothing cannot pass.
    #[test]
    fn files_beside_a_panicking_one_are_still_linted() {
        let directory = tempfile::tempdir().expect("tempdir");
        let plans = plans();
        let inputs = [("bad.py", TRIGGER), ("good.py", "x = 1\n")];

        let outcomes: Vec<_> = inputs
            .par_iter()
            .map(|(name, content)| {
                lint_content(
                    &discovered(&directory.path().join(name)),
                    &plans,
                    content,
                    false,
                    &[],
                    false,
                    &LintPrefill::default(),
                )
            })
            .collect();

        assert!(outcomes[0].is_err(), "the panicking file is reported as a failure");
        let pass = outcomes[1].as_ref().expect("the neighbour must still be linted");
        assert_eq!(pass.diagnostics.len(), 1, "the neighbour's finding must survive");
    }
}

mod lint_batching {
    //! The lint prefill: batch-capable lint engines run once over many files
    //! ahead of the loop, and the loop uses what they produced only when it is
    //! provably the answer for the content in hand.

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::config::{EngineConfig, GlobalDefaults};
    use crate::engine::{BatchSupport, Capabilities, Engine, Severity};
    use crate::filter::SeverityRemap;

    /// Answers `batched` through [`Engine::lint_batch`] and `per-file` through
    /// [`Engine::lint`], so which path produced a diagnostic is visible in it.
    /// A member whose content contains `REFUSED` is declined by the batch.
    struct Batching {
        batches: Arc<AtomicUsize>,
    }

    fn finding(title: &str) -> Diagnostic {
        Diagnostic {
            engine: "batching".to_owned(),
            code: Some("B001".to_owned()),
            severity: Severity::Warning,
            title: title.to_owned(),
            description: None,
            url: None,
            span: None,
            fix: Vec::new(),
            metadata: std::collections::BTreeMap::new(),
        }
    }

    impl Engine for Batching {
        fn name(&self) -> &'static str {
            "batching"
        }

        fn languages(&self) -> &'static [Language] {
            &[Language::Shell]
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                lint: true,
                format: false,
                fix: false,
            }
        }

        fn version(&self) -> &str {
            "test"
        }

        fn batch_support(&self, _cfg: &EngineConfig) -> BatchSupport {
            BatchSupport {
                format: false,
                lint: true,
            }
        }

        fn lint(&self, _src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
            Ok(vec![finding("per-file")])
        }

        fn lint_batch(
            &self,
            batch: &[SourceFile],
            _cfg: &EngineConfig,
        ) -> anyhow::Result<Vec<anyhow::Result<Vec<Diagnostic>>>> {
            self.batches.fetch_add(1, Ordering::SeqCst);
            Ok(batch
                .iter()
                .map(|src| {
                    if src.content.contains("REFUSED") {
                        Err(anyhow::anyhow!("declined"))
                    } else {
                        Ok(vec![finding("batched")])
                    }
                })
                .collect())
        }
    }

    fn plans(batches: &Arc<AtomicUsize>) -> Vec<EnginePlan> {
        vec![EnginePlan {
            engine: Box::new(Batching {
                batches: Arc::clone(batches),
            }),
            config: EngineConfig {
                globals: GlobalDefaults::default(),
                indent_width: 2,
                options: toml::Table::new(),
            },
            serialized_args: ResultCache::serialize_args(&toml::Table::new()),
            severity_remap: SeverityRemap::new(Vec::new()),
            provides_language_lint: true,
            format_ignore: None,
        }]
    }

    fn discovered(path: &std::path::Path) -> DiscoveredFile {
        DiscoveredFile {
            path: path.to_path_buf(),
            language: Language::Shell,
            config_id: 0,
        }
    }

    fn titles(diagnostics: &[Diagnostic]) -> Vec<&str> {
        diagnostics.iter().map(|d| d.title.as_str()).collect()
    }

    #[test]
    fn a_batch_answers_for_the_files_it_covered_and_declined_members_take_the_per_file_path() {
        let directory = tempfile::tempdir().expect("tempdir");
        let contents = [("a.sh", "echo a\n"), ("b.sh", "echo b\n"), ("c.sh", "REFUSED\n")];
        let files: Vec<DiscoveredFile> = contents
            .iter()
            .map(|(name, content)| {
                let path = directory.path().join(name);
                std::fs::write(&path, content).expect("write fixture");
                discovered(&path)
            })
            .collect();
        let batches = Arc::new(AtomicUsize::new(0));
        let plan = RunPlan::serving(0, Language::Shell, plans(&batches));
        let pending: Vec<PendingLint> = files.iter().map(|file| (file, None)).collect();
        let prefilled = prefill_lint(&pending, &plan);

        assert_eq!(
            batches.load(Ordering::SeqCst),
            1,
            "three files must share one batch, not spawn three"
        );
        assert_eq!(prefilled.len(), 2, "the declined member must not get an entry");

        let lint = |index: usize| {
            lint_content(
                &files[index],
                &plans(&batches),
                contents[index].1,
                false,
                &[],
                false,
                &prefilled,
            )
            .expect("lint")
            .diagnostics
        };
        assert_eq!(titles(&lint(0)), ["batched"]);
        assert_eq!(titles(&lint(1)), ["batched"]);
        assert_eq!(titles(&lint(2)), ["per-file"], "a declined member must still be linted");
    }

    /// The mis-attribution guard: an entry is only the answer for the content it
    /// was computed from. A `--fix` pass lints rewritten bytes, and must not be
    /// handed the findings of the bytes before the fix.
    #[test]
    fn an_entry_is_not_applied_to_different_content() {
        let directory = tempfile::tempdir().expect("tempdir");
        let files: Vec<DiscoveredFile> = ["a.sh", "b.sh"]
            .iter()
            .map(|name| {
                let path = directory.path().join(name);
                std::fs::write(&path, "echo $1\n").expect("write fixture");
                discovered(&path)
            })
            .collect();
        let batches = Arc::new(AtomicUsize::new(0));
        let plan = RunPlan::serving(0, Language::Shell, plans(&batches));
        let pending: Vec<PendingLint> = files.iter().map(|file| (file, None)).collect();
        let prefilled = prefill_lint(&pending, &plan);

        let pass = lint_content(
            &files[0],
            &plans(&batches),
            "echo \"$1\"\n",
            false,
            &[],
            false,
            &prefilled,
        )
        .expect("lint");
        assert_eq!(titles(&pass.diagnostics), ["per-file"]);
    }
}
