//! Result-cache eligibility: map a builtin or inline job onto its tier-1
//! [`HookCache`] policy, given the global `[cache.results] hooks` mode.

use anyhow::{Context, Result};
use poly_config::{HookCacheMode, Job};
use poly_hooks::filter::FilePattern;
use poly_hooks::model::HookCache;

/// The result-cache policy for a builtin (`polylint` / `polyfmt`).
///
/// Builtins are deterministic over their matched inputs, so they are cached by
/// matched files in every mode except `Off`.
pub(super) fn builtin_cache(mode: &HookCacheMode) -> HookCache {
    match mode {
        HookCacheMode::Off => HookCache::Disabled,
        HookCacheMode::Safe | HookCacheMode::Aggressive => HookCache::MatchedFiles,
    }
}

/// The result-cache policy for an inline job, resolving the job's `cache.mode`
/// override (else the global `cache_mode`) against its declared `cache.inputs`:
///
/// - `Off` → never cached.
/// - `Safe` + inputs → cached by the declared inputs; no inputs → not cached.
/// - `Aggressive` + inputs → cached by the declared inputs; no inputs → cached
///   by matched files (documented-unsound, opt-in).
///
/// # Errors
///
/// Returns `Err` if a declared cache-input glob fails to compile.
pub(super) fn job_cache(job: &Job, global_mode: &HookCacheMode) -> Result<HookCache> {
    let effective = job
        .cache
        .as_ref()
        .and_then(|cache| cache.mode.clone())
        .unwrap_or_else(|| global_mode.clone());

    let inputs: Vec<String> = job
        .cache
        .as_ref()
        .map(|cache| {
            cache
                .inputs
                .iter()
                .flat_map(|patterns| patterns.as_slice().iter().cloned())
                .collect()
        })
        .unwrap_or_default();

    let declared = |inputs: Vec<String>| -> Result<HookCache> {
        Ok(HookCache::DeclaredInputs(
            FilePattern::glob(inputs).context("invalid hook cache input glob pattern")?,
        ))
    };

    Ok(match effective {
        HookCacheMode::Off => HookCache::Disabled,
        HookCacheMode::Safe => {
            if inputs.is_empty() {
                HookCache::Disabled
            } else {
                declared(inputs)?
            }
        }
        HookCacheMode::Aggressive => {
            if inputs.is_empty() {
                HookCache::MatchedFiles
            } else {
                declared(inputs)?
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use poly_config::{HookCacheMode, HooksConfig, PolyConfig};
    use poly_hooks::Stage as HookStage;
    use poly_hooks::model::HookCache;

    // `super` is the `lower` module, where `lower_stage` lives.
    use super::super::lower_stage;

    fn hooks_from(toml: &str) -> HooksConfig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        std::fs::write(&path, toml).unwrap();
        PolyConfig::load_file(&path).unwrap().hooks
    }

    fn poly() -> PathBuf {
        PathBuf::from("/opt/poly/bin/poly")
    }

    /// Lower a single-stage config under an explicit cache mode and return the
    /// hook with the given id, observing the policy as wired by lowering.
    fn cache_of(
        hooks: &HooksConfig,
        stage: HookStage,
        id: &str,
        mode: &HookCacheMode,
    ) -> HookCache {
        let spec = lower_stage(hooks, &poly(), stage, &[], mode).unwrap();
        spec.hooks
            .into_iter()
            .find(|hook| hook.id == id)
            .unwrap_or_else(|| panic!("hook `{id}` not lowered"))
            .cache
    }

    #[test]
    fn builtin_polylint_caches_matched_files_in_safe_mode() {
        let hooks = hooks_from("[hooks.builtin]\npolylint = true\n");
        let cache = cache_of(
            &hooks,
            HookStage::PreCommit,
            "polylint",
            &HookCacheMode::Safe,
        );
        assert!(matches!(cache, HookCache::MatchedFiles));
    }

    #[test]
    fn builtin_polyfmt_caches_matched_files_in_safe_mode() {
        let hooks = hooks_from("[hooks.builtin]\npolyfmt = true\n");
        let cache = cache_of(
            &hooks,
            HookStage::PreCommit,
            "polyfmt",
            &HookCacheMode::Safe,
        );
        assert!(matches!(cache, HookCache::MatchedFiles));
    }

    #[test]
    fn builtin_is_disabled_in_off_mode() {
        let hooks = hooks_from("[hooks.builtin]\npolylint = true\n");
        let cache = cache_of(
            &hooks,
            HookStage::PreCommit,
            "polylint",
            &HookCacheMode::Off,
        );
        assert!(matches!(cache, HookCache::Disabled));
    }

    #[test]
    fn builtin_commit_is_never_cached() {
        let hooks = hooks_from("[hooks.builtin]\ncommit = true\n");
        // Even in Aggressive mode the commit-msg builtin is disabled (the message
        // content varies per invocation).
        let cache = cache_of(
            &hooks,
            HookStage::CommitMsg,
            "poly-commit",
            &HookCacheMode::Aggressive,
        );
        assert!(matches!(cache, HookCache::Disabled));
    }

    #[test]
    fn safe_inline_job_without_inputs_is_disabled() {
        let hooks = hooks_from(
            r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "j"
run = "x"
"#,
        );
        let cache = cache_of(&hooks, HookStage::PreCommit, "j", &HookCacheMode::Safe);
        assert!(matches!(cache, HookCache::Disabled));
    }

    #[test]
    fn safe_inline_job_with_inputs_uses_declared_inputs() {
        let hooks = hooks_from(
            r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "j"
run = "x"
cache = { inputs = ["**/*.rs", "Cargo.toml"] }
"#,
        );
        let cache = cache_of(&hooks, HookStage::PreCommit, "j", &HookCacheMode::Safe);
        let HookCache::DeclaredInputs(pattern) = cache else {
            panic!("expected DeclaredInputs, got {cache:?}");
        };
        assert!(pattern.is_match(Path::new("src/lib.rs")));
        assert!(pattern.is_match(Path::new("Cargo.toml")));
        assert!(!pattern.is_match(Path::new("README.md")));
    }

    #[test]
    fn aggressive_inline_job_without_inputs_falls_back_to_matched_files() {
        let hooks = hooks_from(
            r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "j"
run = "x"
"#,
        );
        let cache = cache_of(
            &hooks,
            HookStage::PreCommit,
            "j",
            &HookCacheMode::Aggressive,
        );
        assert!(matches!(cache, HookCache::MatchedFiles));
    }

    #[test]
    fn off_mode_disables_inline_job_even_with_inputs() {
        let hooks = hooks_from(
            r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "j"
run = "x"
cache = { inputs = ["**/*.rs"] }
"#,
        );
        let cache = cache_of(&hooks, HookStage::PreCommit, "j", &HookCacheMode::Off);
        assert!(matches!(cache, HookCache::Disabled));
    }

    #[test]
    fn job_cache_mode_override_wins_over_global() {
        // Global Off, but the job opts into Aggressive → cached by matched files.
        let hooks = hooks_from(
            r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "j"
run = "x"
cache = { mode = "aggressive" }
"#,
        );
        let cache = cache_of(&hooks, HookStage::PreCommit, "j", &HookCacheMode::Off);
        assert!(matches!(cache, HookCache::MatchedFiles));
    }
}
