//! Merging selected producer hooks into the native runner's stage model,
//! including the throwaway `[hooks]` config used to lower one remote hook.

use std::path::{Path, PathBuf};

use poly_config::{BuiltinHooks, CargoHooks, HooksConfig, Stage, StageConfig};

use super::select::ResolvedHook;
use crate::hooks::lower;

/// Merge selected producer hooks for `spec.stage` into the native runner model.
pub fn merge_stage(
    spec: &mut poly_hooks::StageSpec,
    hooks: &[ResolvedHook],
    poly_bin: &Path,
    files: &[PathBuf],
    cache_mode: &poly_config::HookCacheMode,
    consumer_root: &Path,
) -> anyhow::Result<()> {
    for selected in hooks {
        if !selected
            .manifest
            .stages
            .iter()
            .any(|stage| lower::to_hook_stage(*stage) == Some(spec.stage))
        {
            continue;
        }
        let mut job = selected.manifest.job.clone();
        job.name = Some(selected.manifest.id.clone());
        job.run = Some(selected.command.clone());
        job.script = None;
        job.env.insert(
            "POLY_HOOK_SOURCE_ROOT".to_string(),
            selected.source_root.to_string_lossy().into_owned(),
        );
        let mut stage = StageConfig::default();
        stage.commands.insert(selected.manifest.id.clone(), job);
        let config = synthetic_config(lower::from_hook_stage(spec.stage), stage);
        let mut lowered = lower::lower_stage(
            &config,
            poly_bin,
            spec.stage,
            files,
            cache_mode,
            consumer_root,
            &poly_config::ToolsConfig::default(),
        )?;
        for hook in &mut lowered.hooks {
            hook.id = format!("{}:{}", selected.source_id, hook.id);
            if let Some(pass_filenames) = selected.manifest.pass_filenames {
                hook.pass_filenames = pass_filenames;
            }
            if let Some(always_run) = selected.manifest.always_run {
                hook.always_run = always_run;
            }
        }
        spec.hooks.extend(lowered.hooks);
    }
    spec.hooks.sort_by_key(|hook| hook.priority);
    Ok(())
}

/// The throwaway `[hooks]` config used to lower a single selected remote hook.
///
/// A remote catalog contributes only the hooks the consumer selected from it —
/// it must never re-derive the consumer's own builtins. `lower_stage` emits
/// `[hooks.builtin]` hooks alongside the stage's jobs, and the `cargo` group is
/// default-on whenever a `[hooks]` section is present, so lowering through a
/// `present = true` config duplicated `cargo-clippy`/`sort`/`machete`/`deny`
/// under the `{source}:` prefix once per `[[hooks.sources]]` entry. Every
/// builtin is therefore pinned off explicitly instead of being left to
/// `HooksConfig::default()`, whose `cargo: None` means "on when a `[hooks]`
/// section exists" rather than "off". Catalog tools are kept out the same way,
/// by lowering against an empty `ToolsConfig`.
fn synthetic_config(stage: Stage, stage_config: StageConfig) -> HooksConfig {
    let mut config = HooksConfig {
        present: false,
        builtin: BuiltinHooks {
            cargo: Some(CargoHooks {
                enabled: false,
                ..CargoHooks::default()
            }),
            ..BuiltinHooks::default()
        },
        ..HooksConfig::default()
    };
    config.stage_configs.insert(stage, stage_config);
    config
}

#[cfg(test)]
mod tests {
    use super::super::manifest::PRODUCER_MANIFEST_NAME;
    use super::super::provision::provision;
    use super::super::test_support::{preferences, write_catalog, write_consumer};
    use super::*;

    #[test]
    fn lowers_catalog_args_and_filename_controls() {
        let consumer = tempfile::tempdir().unwrap();
        let producer = tempfile::tempdir().unwrap();
        std::fs::write(
            producer.path().join(PRODUCER_MANIFEST_NAME),
            r#"
version = 1
[[hooks]]
id = "validate"
stages = ["pre-commit"]
args = ["generate", "--dry-run"]
workspace = true
pass_filenames = false
always_run = true
[[hooks.paths]]
channel = "shell"
check = "command -v printf"
run = "printf"
"#,
        )
        .unwrap();
        preferences(consumer.path());
        let config = write_consumer(consumer.path(), producer.path(), &["validate"]);
        let selected = provision(consumer.path(), &config, false, true).unwrap();
        let mut spec = poly_hooks::StageSpec {
            stage: poly_hooks::Stage::PreCommit,
            ..poly_hooks::StageSpec::default()
        };
        merge_stage(
            &mut spec,
            &selected,
            Path::new("poly"),
            &[],
            &poly_config::HookCacheMode::Off,
            consumer.path(),
        )
        .unwrap();
        assert_eq!(spec.hooks.len(), 1);
        assert_eq!(spec.hooks[0].args, ["generate", "--dry-run"]);
        assert!(!spec.hooks[0].pass_filenames);
        assert!(spec.hooks[0].always_run);
        assert!(spec.hooks[0].workspace);
        assert!(spec.hooks[0].cwd.is_none());
        assert_eq!(
            spec.hooks[0].env.get("POLY_HOOK_SOURCE_ROOT"),
            Some(&producer.path().canonicalize().unwrap().to_string_lossy().into_owned())
        );
    }

    /// A remote source contributes exactly the hooks it selects — never the
    /// consumer's builtins.
    ///
    /// `lower_stage` also emits `[hooks.builtin]` entries, and the `cargo` group
    /// is default-on whenever a `[hooks]` section is present, so a synthetic
    /// config carrying `present = true` re-emitted `cargo-clippy`/`sort`/
    /// `machete`/`deny` once per source. The consumer root here carries a
    /// `Cargo.toml` so that default-on gate is live (the group is additionally
    /// `PATH`-probed, so the duplication only surfaces where the tools exist).
    #[test]
    fn remote_source_contributes_only_its_selected_hooks() {
        let consumer = tempfile::tempdir().unwrap();
        let producer = tempfile::tempdir().unwrap();
        std::fs::write(consumer.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        write_catalog(producer.path());
        preferences(consumer.path());
        let config = write_consumer(consumer.path(), producer.path(), &["validate"]);
        let selected = provision(consumer.path(), &config, false, true).unwrap();
        let mut spec = poly_hooks::StageSpec {
            stage: poly_hooks::Stage::PreCommit,
            ..poly_hooks::StageSpec::default()
        };

        merge_stage(
            &mut spec,
            &selected,
            Path::new("poly"),
            &[],
            &poly_config::HookCacheMode::Off,
            consumer.path(),
        )
        .unwrap();

        let ids: Vec<&str> = spec.hooks.iter().map(|hook| hook.id.as_str()).collect();
        assert_eq!(ids, ["rules:validate"]);
    }
}
