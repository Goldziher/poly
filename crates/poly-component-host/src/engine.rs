//! [`ComponentEngine`]: an [`Engine`] whose per-file work happens in a component.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use poly_component_protocol::{
    Payload, Request, WireBatchSupport, WireDiagnostic, WireEngineDesc, WireFile, WireFileSlice, WireFormatOutput,
    WireKind, WireOptionTable, WireProjectInputs, WireSelection, hooks,
};
use poly_engine_api::component_wire::OffsetOutOfRange;
use poly_engine_api::config::EngineConfig;
use poly_engine_api::engine::{
    BatchSupport, Capabilities, Diagnostic, Engine, FormatOutput, OptionKeys, OptionTable, OptionType, ProjectDirs,
    ProjectInputs, ProjectRequest, ProjectSideFile, SourceFile,
};
use poly_engine_api::host_hooks::HostHook;
use poly_engine_api::language::Language;

use crate::slot::{ComponentSlot, Connection, wire_error_to_anyhow};

/// Hex digits of the lock digest folded into [`Engine::version`].
const LOCK_DIGEST_LEN: usize = 16;

/// `<index version>+c<lock sha256[..16]>`: the version a component engine reports.
///
/// Computed from the index and the lock alone, so it needs no load, and a different artifact in
/// the lock yields a different cache key.
#[must_use]
pub fn component_version(index_version: &str, lock_sha256: &str) -> String {
    let end = lock_sha256
        .char_indices()
        .nth(LOCK_DIGEST_LEN)
        .map_or(lock_sha256.len(), |(i, _)| i);
    format!("{index_version}+c{}", &lock_sha256[..end])
}

/// An [`Engine`] backed by an engine hosted in a component.
///
/// Three sources answer its methods: the index (static metadata, decided at construction), the
/// host-side cache hooks of [`poly_engine_api::host_hooks`] (never a transport call, so a warm
/// run does not load the component) and the component itself (everything per file).
///
/// The name, languages and option keys are leaked: an engine is built once per component per
/// process and lives for the rest of it, and the [`Engine`] trait demands `'static` data.
#[derive(Clone)]
pub struct ComponentEngine {
    name: &'static str,
    languages: &'static [Language],
    capabilities: Capabilities,
    version: String,
    option_keys: [OptionKeys; 3],
    hooks: u32,
    host_hook: HostHook,
    takes_over_tier_one: bool,
    claimed_paths: Vec<String>,
    supplements_tier_one: bool,
    self_manages_enabled: bool,
    supersedes_generic_formatter: bool,
    batch_shard_floor: usize,
    project_inputs: ProjectInputs,
    selection: Option<WireSelection>,
    slot: Arc<ComponentSlot>,
    unavailable: Option<String>,
    unavailable_skips: bool,
    unavailable_noop: bool,
}

fn table_slot(table: OptionTable) -> usize {
    match table {
        OptionTable::Lint => 0,
        OptionTable::Format => 1,
        OptionTable::CrossCuttingLint => 2,
    }
}

fn wire_table(table: WireOptionTable) -> OptionTable {
    match table {
        WireOptionTable::Lint => OptionTable::Lint,
        WireOptionTable::Format => OptionTable::Format,
        WireOptionTable::CrossCuttingLint => OptionTable::CrossCuttingLint,
    }
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

fn project_inputs(wire: &WireProjectInputs) -> ProjectInputs {
    ProjectInputs {
        gate_key: wire.gate_key.clone(),
        files: wire
            .files
            .iter()
            .map(|file| ProjectSideFile {
                option_key: file.option_key.clone(),
                default_path: file.default_path.clone(),
            })
            .collect(),
        dirs: wire.dirs.as_ref().map(|dirs| ProjectDirs {
            dir_key: dirs.dir_key.clone(),
            dir_default: dirs.dir_default.clone(),
            paths_key: dirs.paths_key.clone(),
            recursive_key: dirs.recursive_key.clone(),
            list_unserved: dirs.list_unserved,
        }),
    }
}

fn build_option_keys(component_id: &str, engine: &str, desc: &WireEngineDesc) -> anyhow::Result<[OptionKeys; 3]> {
    let mut keys = [OptionKeys::UNCHECKED; 3];
    for wire in &desc.option_keys {
        if !wire.checked {
            continue;
        }
        let declared = wire
            .declared
            .iter()
            .map(|key| {
                let option_type = OptionType::from_bits(key.types)
                    .with_context(|| format!("option key `{}` declares invalid type bits {}", key.name, key.types))?;
                Ok((leak(&key.name), option_type))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut built = OptionKeys::declared(Box::leak(declared.into_boxed_slice()));
        if wire.rule_selection {
            built = built.with_rule_selection();
        }
        if wire.has_derived {
            built = built.with_component_derived(leak(component_id), leak(engine));
        }
        if let Some(note) = &wire.note {
            built = built.with_note(leak(note));
        }
        keys[table_slot(wire_table(wire.table))] = built;
    }
    Ok(keys)
}

impl ComponentEngine {
    /// Build the proxy for one engine of a component from its index entry.
    ///
    /// `lock_sha256` is the component's lock digest (see [`component_version`]).
    ///
    /// # Errors
    ///
    /// When the index entry is malformed: an unknown `host_hook` or invalid option type bits.
    pub fn new(
        component_id: &str,
        desc: &WireEngineDesc,
        lock_sha256: &str,
        slot: Arc<ComponentSlot>,
    ) -> anyhow::Result<Self> {
        let host_hook: HostHook = serde_json::from_value(serde_json::Value::String(desc.host_hook.clone()))
            .with_context(|| format!("engine '{}' names unknown host hook `{}`", desc.name, desc.host_hook))?;
        let languages: Vec<Language> = desc
            .languages
            .iter()
            .map(|id| Language::from_catalog_name(id))
            .collect();
        Ok(Self {
            name: leak(&desc.name),
            languages: Box::leak(languages.into_boxed_slice()),
            capabilities: Capabilities {
                lint: desc.capabilities.lint,
                format: desc.capabilities.format,
                fix: desc.capabilities.fix,
                project: desc.capabilities.project,
            },
            version: component_version(&desc.version, lock_sha256),
            option_keys: build_option_keys(component_id, &desc.name, desc)?,
            hooks: desc.hooks,
            host_hook,
            takes_over_tier_one: desc.takes_over_tier_one,
            claimed_paths: desc.claimed_paths.clone(),
            supplements_tier_one: desc.supplements_tier_one,
            self_manages_enabled: desc.self_manages_enabled,
            supersedes_generic_formatter: desc.supersedes_generic_formatter,
            batch_shard_floor: usize::try_from(desc.batch_shard_floor).unwrap_or(usize::MAX),
            project_inputs: project_inputs(&desc.project_inputs),
            selection: desc.selection.clone(),
            slot,
            unavailable: None,
            unavailable_skips: false,
            unavailable_noop: false,
        })
    }

    /// The stand-in for an engine whose component cannot be used (no artifact for this target,
    /// offline with nothing cached, a failed load).
    ///
    /// It answers statics from the index like any component engine but claims no language
    /// coverage, reports `reason` as the skip reason of every file, and fails every call with
    /// `reason`. Its version carries an `+unavailable` marker so nothing it records can be
    /// replayed under the real engine's cache key.
    ///
    /// # Errors
    ///
    /// When the index entry is malformed, as for [`ComponentEngine::new`].
    pub fn unavailable(
        component_id: &str,
        desc: &WireEngineDesc,
        lock_sha256: &str,
        reason: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let reason = reason.into();
        let slot = ComponentSlot::failed(component_id, reason.clone());
        let mut engine = Self::new(component_id, desc, lock_sha256, slot)?;
        engine.version.push_str("+unavailable");
        engine.unavailable = Some(reason);
        engine.unavailable_skips = true;
        Ok(engine)
    }

    /// Like [`ComponentEngine::unavailable`], but the engine only declines: it reports `reason`
    /// as the skip reason and otherwise does nothing, so a lint or format call succeeds with no
    /// findings and no change instead of failing. For a component that is absent by policy
    /// (offline, install never), where the files are reported as skipped and nothing else.
    ///
    /// # Errors
    ///
    /// When the index entry is malformed, as for [`ComponentEngine::new`].
    pub fn unavailable_declining(
        component_id: &str,
        desc: &WireEngineDesc,
        lock_sha256: &str,
        reason: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let mut engine = Self::unavailable(component_id, desc, lock_sha256, reason)?;
        engine.unavailable_noop = true;
        Ok(engine)
    }

    /// Like [`ComponentEngine::unavailable`], but the engine declines nothing: it takes every
    /// file and fails it with `reason`, so a run reports each one as an error instead of a skip.
    ///
    /// # Errors
    ///
    /// When the index entry is malformed, as for [`ComponentEngine::new`].
    pub fn unavailable_failing(
        component_id: &str,
        desc: &WireEngineDesc,
        lock_sha256: &str,
        reason: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let mut engine = Self::unavailable(component_id, desc, lock_sha256, reason)?;
        engine.unavailable_skips = false;
        Ok(engine)
    }

    /// Why the component cannot be used, for an engine built by [`ComponentEngine::unavailable`].
    #[must_use]
    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// The reason, for an engine that declines its files because the component is absent by
    /// policy ([`ComponentEngine::unavailable_declining`]); `None` for a usable engine and for
    /// a failing stand-in.
    #[must_use]
    pub fn declining_reason(&self) -> Option<&str> {
        self.unavailable.as_deref().filter(|_| self.unavailable_skips)
    }

    /// The slot this engine loads its component through.
    #[must_use]
    pub fn slot(&self) -> &Arc<ComponentSlot> {
        &self.slot
    }

    fn hook(&self, bit: u32) -> bool {
        self.hooks & bit != 0
    }

    fn engine(&self) -> String {
        self.name.to_owned()
    }

    fn connect(&self) -> anyhow::Result<Arc<Connection>> {
        self.slot.connection()
    }

    fn unexpected(&self, request: &str) -> anyhow::Error {
        anyhow::anyhow!(
            "component answered {request} for '{}' with an unexpected payload",
            self.name
        )
    }
}

fn wire_file(src: &SourceFile) -> WireFile {
    WireFile {
        path: src.path.display().to_string(),
        language: src.language.id().to_owned(),
        len: src.content.len() as u64,
    }
}

fn slices(batch: &[SourceFile]) -> (Vec<WireFileSlice>, Vec<u8>) {
    let mut body = Vec::with_capacity(batch.iter().map(|f| f.content.len()).sum());
    let files = batch
        .iter()
        .map(|file| {
            let slice = WireFileSlice {
                path: file.path.display().to_string(),
                language: file.language.id().to_owned(),
                offset: body.len() as u64,
                len: file.content.len() as u64,
            };
            body.extend_from_slice(file.content.as_bytes());
            slice
        })
        .collect();
    (files, body)
}

fn diagnostics(wire: Vec<WireDiagnostic>) -> anyhow::Result<Vec<Diagnostic>> {
    wire.into_iter()
        .map(|d| Diagnostic::try_from(d).map_err(|e: OffsetOutOfRange| anyhow::Error::new(e)))
        .collect()
}

fn format_output(wire: WireFormatOutput) -> FormatOutput {
    match wire {
        WireFormatOutput::Unchanged => FormatOutput::Unchanged,
        WireFormatOutput::Formatted(text) => FormatOutput::Formatted(text),
    }
}

impl Engine for ComponentEngine {
    fn name(&self) -> &'static str {
        self.name
    }

    fn languages(&self) -> &'static [Language] {
        self.languages
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn option_keys(&self, table: OptionTable) -> OptionKeys {
        self.option_keys[table_slot(table)]
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn host_hook(&self) -> HostHook {
        self.host_hook
    }

    fn provides_language_lint(&self, language: &Language, cfg: &EngineConfig) -> bool {
        if self.unavailable.is_some() {
            return false;
        }
        if !self.hook(hooks::PROVIDES_LANGUAGE) {
            return !self.languages.is_empty();
        }
        self.provides_language(language, cfg, WireKind::Lint)
    }

    fn provides_language_format(&self, language: &Language, cfg: &EngineConfig) -> bool {
        if self.unavailable.is_some() {
            return false;
        }
        if !self.hook(hooks::PROVIDES_LANGUAGE) {
            return self.capabilities.format;
        }
        self.provides_language(language, cfg, WireKind::Format)
    }

    fn skip_reason(&self, src: &SourceFile) -> Option<Cow<'static, str>> {
        if let Some(reason) = &self.unavailable {
            return self.unavailable_skips.then(|| Cow::Owned(reason.clone()));
        }
        if !self.hook(hooks::SKIP_REASON) {
            return None;
        }
        let call = || -> anyhow::Result<Option<String>> {
            let request = Request::SkipReason {
                engine: self.engine(),
                file: wire_file(src),
            };
            match self
                .connect()?
                .call_plain(self.name, &request, src.content.as_bytes())?
            {
                Payload::OptString(reason) => Ok(reason),
                _ => Err(self.unexpected("SkipReason")),
            }
        };
        match call() {
            Ok(reason) => reason.map(Cow::Owned),
            Err(error) => {
                tracing::warn!(engine = self.name, "skip_reason unavailable: {error:#}");
                None
            }
        }
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        if self.unavailable_noop {
            return Ok(Vec::new());
        }
        let connection = self.connect()?;
        let file = wire_file(src);
        let payload = connection.call(
            self.name,
            cfg,
            |config| Request::Lint {
                engine: self.engine(),
                config,
                file: file.clone(),
            },
            src.content.as_bytes(),
        )?;
        match payload {
            Payload::Diagnostics(found) => diagnostics(found),
            _ => Err(self.unexpected("Lint")),
        }
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        if self.unavailable_noop {
            return Ok(FormatOutput::Unchanged);
        }
        let connection = self.connect()?;
        let file = wire_file(src);
        let payload = connection.call(
            self.name,
            cfg,
            |config| Request::Format {
                engine: self.engine(),
                config,
                file: file.clone(),
            },
            src.content.as_bytes(),
        )?;
        match payload {
            Payload::Formatted(output) => Ok(format_output(output)),
            _ => Err(self.unexpected("Format")),
        }
    }

    fn project_check(&self, req: &ProjectRequest<'_>) -> anyhow::Result<Vec<(PathBuf, Vec<Diagnostic>)>> {
        if self.unavailable_noop {
            return Ok(Vec::new());
        }
        let connection = self.connect()?;
        let relative = |path: &Path| path.strip_prefix(req.root).unwrap_or(path).to_path_buf();
        let mut body = Vec::new();
        let mut files = Vec::with_capacity(req.files.len());
        let mut originals: HashMap<String, PathBuf> = HashMap::with_capacity(req.files.len());
        for file in req.files {
            let rel = relative(&file.path).display().to_string();
            files.push(WireFileSlice {
                path: rel.clone(),
                language: file.language.id().to_owned(),
                offset: body.len() as u64,
                len: file.content.len() as u64,
            });
            body.extend_from_slice(file.content.as_bytes());
            originals.insert(rel, file.path.clone());
        }
        let environment = req.environment.to_wire_pairs();
        let root = req.root.display().to_string();
        let payload = connection.call(
            self.name,
            req.config,
            |config| Request::ProjectCheck {
                engine: self.engine(),
                root: root.clone(),
                files: files.clone(),
                config,
                environment: environment.clone(),
            },
            &body,
        )?;
        let Payload::Project(per_file) = payload else {
            return Err(self.unexpected("ProjectCheck"));
        };
        per_file
            .into_iter()
            .map(|entry| {
                let path = originals
                    .get(&entry.path)
                    .cloned()
                    .unwrap_or_else(|| req.root.join(&entry.path));
                Ok((path, diagnostics(entry.diagnostics)?))
            })
            .collect()
    }

    fn batch_support(&self, cfg: &EngineConfig) -> BatchSupport {
        if self.unavailable.is_some() || !self.hook(hooks::BATCH_SUPPORT) {
            return BatchSupport::default();
        }
        let call = || -> anyhow::Result<WireBatchSupport> {
            let payload = self.connect()?.call(
                self.name,
                cfg,
                |config| Request::BatchSupport {
                    engine: self.engine(),
                    config,
                },
                &[],
            )?;
            match payload {
                Payload::BatchSupport(support) => Ok(support),
                _ => Err(self.unexpected("BatchSupport")),
            }
        };
        match call() {
            Ok(support) => BatchSupport {
                format: support.format,
                lint: support.lint,
            },
            Err(error) => {
                tracing::warn!(engine = self.name, "batch_support unavailable: {error:#}");
                BatchSupport::default()
            }
        }
    }

    fn format_batch(
        &self,
        batch: &[SourceFile],
        cfg: &EngineConfig,
    ) -> anyhow::Result<Vec<anyhow::Result<FormatOutput>>> {
        let connection = self.connect()?;
        let (files, body) = slices(batch);
        let payload = connection.call(
            self.name,
            cfg,
            |config| Request::FormatBatch {
                engine: self.engine(),
                config,
                files: files.clone(),
            },
            &body,
        )?;
        let Payload::FormatBatch(items) = payload else {
            return Err(self.unexpected("FormatBatch"));
        };
        anyhow::ensure!(
            items.len() == batch.len(),
            "component returned {} results for a batch of {}",
            items.len(),
            batch.len()
        );
        Ok(items
            .into_iter()
            .map(|item| match item {
                poly_component_protocol::WireFormatItem::Ok(output) => Ok(format_output(output)),
                poly_component_protocol::WireFormatItem::Err(error) => Err(wire_error_to_anyhow(self.name, error)),
            })
            .collect())
    }

    fn lint_batch(
        &self,
        batch: &[SourceFile],
        cfg: &EngineConfig,
    ) -> anyhow::Result<Vec<anyhow::Result<Vec<Diagnostic>>>> {
        let connection = self.connect()?;
        let (files, body) = slices(batch);
        let payload = connection.call(
            self.name,
            cfg,
            |config| Request::LintBatch {
                engine: self.engine(),
                config,
                files: files.clone(),
            },
            &body,
        )?;
        let Payload::LintBatch(items) = payload else {
            return Err(self.unexpected("LintBatch"));
        };
        anyhow::ensure!(
            items.len() == batch.len(),
            "component returned {} results for a batch of {}",
            items.len(),
            batch.len()
        );
        Ok(items
            .into_iter()
            .map(|item| match item {
                poly_component_protocol::WireLintItem::Ok(found) => diagnostics(found),
                poly_component_protocol::WireLintItem::Err(error) => Err(wire_error_to_anyhow(self.name, error)),
            })
            .collect())
    }

    fn lint_cache_salt(&self, src: &SourceFile) -> Option<String> {
        if self.unavailable.is_some() || !self.hook(hooks::LINT_CACHE_SALT) {
            return None;
        }
        let call = || -> anyhow::Result<Option<String>> {
            let request = Request::LintCacheSalt {
                engine: self.engine(),
                file: wire_file(src),
            };
            match self
                .connect()?
                .call_plain(self.name, &request, src.content.as_bytes())?
            {
                Payload::OptString(salt) => Ok(salt),
                _ => Err(self.unexpected("LintCacheSalt")),
            }
        };
        call().unwrap_or_else(|error| {
            tracing::warn!(engine = self.name, "lint_cache_salt unavailable: {error:#}");
            None
        })
    }

    fn selection(&self) -> Option<WireSelection> {
        self.selection.clone()
    }

    fn batch_shard_floor(&self) -> usize {
        self.batch_shard_floor
    }

    fn project_inputs(&self) -> ProjectInputs {
        self.project_inputs.clone()
    }

    fn takes_over_tier_one(&self) -> bool {
        self.takes_over_tier_one
    }

    fn claimed_paths(&self) -> Vec<String> {
        self.claimed_paths.clone()
    }

    fn supplements_tier_one(&self) -> bool {
        self.supplements_tier_one
    }

    fn self_manages_enabled(&self) -> bool {
        self.self_manages_enabled
    }

    fn supersedes_generic_formatter(&self) -> bool {
        self.supersedes_generic_formatter
    }
}

impl ComponentEngine {
    fn provides_language(&self, language: &Language, cfg: &EngineConfig, kind: WireKind) -> bool {
        let call = || -> anyhow::Result<bool> {
            let payload = self.connect()?.call(
                self.name,
                cfg,
                |config| Request::ProvidesLanguage {
                    engine: self.engine(),
                    config,
                    language: language.id().to_owned(),
                    kind,
                },
                &[],
            )?;
            match payload {
                Payload::Bool(answer) => Ok(answer),
                _ => Err(self.unexpected("ProvidesLanguage")),
            }
        };
        // A claim of coverage the run relies on is never made speculatively.
        call().unwrap_or_else(|error| {
            tracing::warn!(engine = self.name, "provides_language unavailable: {error:#}");
            false
        })
    }
}
