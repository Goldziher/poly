//! Hand named files to a more specific formatter.
//!
//! `pyproject-fmt` formats `pyproject.toml` with opinions of its own (key order,
//! array layout), and the language's tier-one formatter (`taplo`) formats every
//! TOML file with different ones. Run in a chain the two never settle: each
//! rewrites what the other produced. [`FormatScope`] wraps the tier-one formatter
//! and makes it decline the files the specific formatter claims, so each file has
//! exactly one formatter. Everything but formatting those files is delegated
//! unchanged — the wrapped engine still lints them.

use globset::GlobSet;

use crate::config::EngineConfig;
use crate::engine::{
    BatchSupport, Capabilities, Diagnostic, Engine, FormatOutput, OptionKeys, OptionTable, SourceFile,
};
use crate::language::Language;

/// An engine that does not format the files `claimed` matches.
pub struct FormatScope {
    inner: Box<dyn Engine>,
    claimed: GlobSet,
}

impl FormatScope {
    /// Wrap `inner` so it leaves the files matching `claimed` to someone else.
    pub fn new(inner: Box<dyn Engine>, claimed: GlobSet) -> Self {
        FormatScope { inner, claimed }
    }

    fn claims(&self, src: &SourceFile) -> bool {
        self.claimed.is_match(&src.path)
    }
}

impl Engine for FormatScope {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn languages(&self) -> &'static [Language] {
        self.inner.languages()
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn option_keys(&self, table: OptionTable) -> OptionKeys {
        self.inner.option_keys(table)
    }

    fn version(&self) -> &str {
        self.inner.version()
    }

    fn provides_language_lint(&self, language: &Language, cfg: &EngineConfig) -> bool {
        self.inner.provides_language_lint(language, cfg)
    }

    fn provides_language_format(&self, language: &Language, cfg: &EngineConfig) -> bool {
        self.inner.provides_language_format(language, cfg)
    }

    fn skip_reason(&self, src: &SourceFile) -> Option<&'static str> {
        if self.claims(src) {
            return None;
        }
        self.inner.skip_reason(src)
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        self.inner.lint(src, cfg)
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        if self.claims(src) {
            return Ok(FormatOutput::Unchanged);
        }
        self.inner.format(src, cfg)
    }

    fn batch_support(&self, cfg: &EngineConfig) -> BatchSupport {
        self.inner.batch_support(cfg)
    }

    fn format_batch(
        &self,
        batch: &[SourceFile],
        cfg: &EngineConfig,
    ) -> anyhow::Result<Vec<anyhow::Result<FormatOutput>>> {
        let kept: Vec<SourceFile> = batch.iter().filter(|src| !self.claims(src)).cloned().collect();
        let mut formatted = self.inner.format_batch(&kept, cfg)?.into_iter();
        Ok(batch
            .iter()
            .map(|src| {
                if self.claims(src) {
                    Ok(FormatOutput::Unchanged)
                } else {
                    formatted
                        .next()
                        .unwrap_or_else(|| Err(anyhow::anyhow!("batch answered fewer files than it was given")))
                }
            })
            .collect())
    }

    fn lint_batch(
        &self,
        batch: &[SourceFile],
        cfg: &EngineConfig,
    ) -> anyhow::Result<Vec<anyhow::Result<Vec<Diagnostic>>>> {
        self.inner.lint_batch(batch, cfg)
    }

    fn lint_cache_salt(&self, src: &SourceFile) -> Option<String> {
        self.inner.lint_cache_salt(src)
    }

    fn batch_shard_floor(&self) -> usize {
        self.inner.batch_shard_floor()
    }

    fn self_manages_enabled(&self) -> bool {
        self.inner.self_manages_enabled()
    }

    fn supersedes_generic_formatter(&self) -> bool {
        self.inner.supersedes_generic_formatter()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use globset::{Glob, GlobSetBuilder};

    use super::*;
    use crate::config::GlobalDefaults;

    /// Upper-cases every file it formats.
    struct Upper;

    impl Engine for Upper {
        fn name(&self) -> &'static str {
            "upper"
        }

        fn languages(&self) -> &'static [Language] {
            &[Language::Toml]
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                lint: false,
                format: true,
                fix: false,
            }
        }

        fn version(&self) -> &str {
            "test"
        }

        fn format(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
            Ok(FormatOutput::Formatted(src.content.to_uppercase()))
        }

        fn format_batch(
            &self,
            batch: &[SourceFile],
            cfg: &EngineConfig,
        ) -> anyhow::Result<Vec<anyhow::Result<FormatOutput>>> {
            Ok(batch.iter().map(|src| self.format(src, cfg)).collect())
        }
    }

    fn scoped() -> FormatScope {
        let mut builder = GlobSetBuilder::new();
        builder.add(Glob::new("**/pyproject.toml").expect("glob"));
        FormatScope::new(Box::new(Upper), builder.build().expect("set"))
    }

    fn source(path: &str) -> SourceFile {
        SourceFile {
            path: path.into(),
            language: Language::Toml,
            content: Arc::from("a = 1\n"),
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
    fn a_claimed_file_is_left_to_the_specific_formatter() {
        let engine = scoped();
        assert!(matches!(
            engine.format(&source("svc/pyproject.toml"), &cfg()),
            Ok(FormatOutput::Unchanged)
        ));
        assert!(matches!(
            engine.format(&source("svc/other.toml"), &cfg()),
            Ok(FormatOutput::Formatted(text)) if text == "A = 1\n"
        ));
    }

    #[test]
    fn a_batch_keeps_its_order_around_the_claimed_files() {
        let engine = scoped();
        let batch = [source("a.toml"), source("x/pyproject.toml"), source("b.toml")];
        let out = engine.format_batch(&batch, &cfg()).expect("batch");
        assert_eq!(out.len(), 3);
        assert!(matches!(&out[0], Ok(FormatOutput::Formatted(_))));
        assert!(matches!(&out[1], Ok(FormatOutput::Unchanged)));
        assert!(matches!(&out[2], Ok(FormatOutput::Formatted(_))));
    }

    #[test]
    fn identity_is_the_wrapped_engines() {
        let engine = scoped();
        assert_eq!(engine.name(), "upper");
        assert_eq!(engine.version(), "test");
        assert!(engine.capabilities().format);
    }
}
