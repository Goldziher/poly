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
