//! Routing one decoded [`Request`] to the engine it names.

use std::path::{Path, PathBuf};

use poly_component_protocol::{
    PROTOCOL_VERSION, Payload, Request, WireBatchSupport, WireDiagnostic, WireError, WireErrorKind,
    WireFileDiagnostics, WireFormatItem, WireFormatOutput, WireKind, WireLintItem, WireOptionTable, to_toml,
};
use poly_engine_api::engine::{Engine, FormatOutput, ProjectEnvironment, ProjectFile, ProjectRequest};

use super::describe::TABLES;
use super::errors::{from_anyhow, guarded, protocol};
use super::files::{single, slices};
use super::language::language_from_id;
use super::{InitState, Producer};
use poly_engine_api::component_wire::engine_config_from_wire;

fn diagnostics(found: &[poly_engine_api::engine::Diagnostic]) -> Vec<WireDiagnostic> {
    found.iter().map(WireDiagnostic::from).collect()
}

fn format_output(output: FormatOutput) -> WireFormatOutput {
    match output {
        FormatOutput::Unchanged => WireFormatOutput::Unchanged,
        FormatOutput::Formatted(text) => WireFormatOutput::Formatted(text),
    }
}

/// A batch answer must have one entry per member, or the host would pair results with the wrong files.
fn check_batch_len(operation: &str, results: usize, members: usize) -> Result<(), WireError> {
    if results == members {
        Ok(())
    } else {
        Err(from_anyhow(&anyhow::anyhow!(
            "{operation} returned {results} result(s) for {members} file(s)"
        )))
    }
}

impl Producer {
    pub(super) fn dispatch(&self, request: Request, body: &[u8]) -> Result<Payload, WireError> {
        if let Request::Init {
            protocol: host_protocol,
            host_version,
            threads,
            stack_size,
            max_log_level,
        } = request
        {
            return self.init(host_protocol, host_version, threads, stack_size, max_log_level);
        }
        if self.init.get().is_none() {
            return Err(protocol("the first call must be Init"));
        }
        match request {
            Request::Init { .. } => unreachable!("handled above"),
            Request::RegisterConfig { id, config } => {
                self.register(id, engine_config_from_wire(&config)?);
                Ok(Payload::Ok)
            }
            Request::Lint { engine, config, file } => {
                let engine = self.engine(&engine, Some(&file.language))?;
                let cfg = self.config(config)?;
                let src = single(&file, body)?;
                let found = guarded(engine.name(), || engine.lint(&src, &cfg))?;
                Ok(Payload::Diagnostics(diagnostics(&found)))
            }
            Request::Format { engine, config, file } => {
                let engine = self.engine(&engine, Some(&file.language))?;
                let cfg = self.config(config)?;
                let src = single(&file, body)?;
                let output = guarded(engine.name(), || engine.format(&src, &cfg))?;
                Ok(Payload::Formatted(format_output(output)))
            }
            Request::LintBatch { engine, config, files } => {
                let engine = self.engine(&engine, files.first().map(|f| f.language.as_str()))?;
                let cfg = self.config(config)?;
                let batch = slices(&files, body)?;
                let results = guarded(engine.name(), || engine.lint_batch(&batch, &cfg))?;
                check_batch_len("lint_batch", results.len(), batch.len())?;
                Ok(Payload::LintBatch(
                    results
                        .into_iter()
                        .map(|item| match item {
                            Ok(found) => WireLintItem::Ok(diagnostics(&found)),
                            Err(error) => WireLintItem::Err(from_anyhow(&error)),
                        })
                        .collect(),
                ))
            }
            Request::FormatBatch { engine, config, files } => {
                let engine = self.engine(&engine, files.first().map(|f| f.language.as_str()))?;
                let cfg = self.config(config)?;
                let batch = slices(&files, body)?;
                let results = guarded(engine.name(), || engine.format_batch(&batch, &cfg))?;
                check_batch_len("format_batch", results.len(), batch.len())?;
                Ok(Payload::FormatBatch(
                    results
                        .into_iter()
                        .map(|item| match item {
                            Ok(output) => WireFormatItem::Ok(format_output(output)),
                            Err(error) => WireFormatItem::Err(from_anyhow(&error)),
                        })
                        .collect(),
                ))
            }
            Request::ProjectCheck {
                engine,
                root,
                files,
                config,
                environment,
            } => {
                let engine = self.engine(&engine, None)?;
                let cfg = self.config(config)?;
                let members: Vec<ProjectFile> = slices(&files, body)?
                    .into_iter()
                    .map(|src| ProjectFile {
                        path: src.path,
                        language: src.language,
                        content: src.content,
                    })
                    .collect();
                let environment = ProjectEnvironment::from_wire_pairs(&environment);
                let root = PathBuf::from(root);
                let request = ProjectRequest {
                    root: &root,
                    files: &members,
                    config: &cfg,
                    environment: &environment,
                };
                let per_file = guarded(engine.name(), || engine.project_check(&request))?;
                Ok(Payload::Project(
                    per_file
                        .iter()
                        .map(|(path, found)| WireFileDiagnostics {
                            path: path.display().to_string(),
                            diagnostics: diagnostics(found),
                        })
                        .collect(),
                ))
            }
            Request::SkipReason { engine, file } => {
                let engine = self.engine(&engine, Some(&file.language))?;
                let src = single(&file, body)?;
                let reason = guarded(engine.name(), || Ok(engine.skip_reason(&src)))?;
                Ok(Payload::OptString(reason.map(std::borrow::Cow::into_owned)))
            }
            Request::LintCacheSalt { engine, file } => {
                let engine = self.engine(&engine, Some(&file.language))?;
                let src = single(&file, body)?;
                let salt = guarded(engine.name(), || Ok(engine.lint_cache_salt(&src)))?;
                Ok(Payload::OptString(salt))
            }
            Request::ProvidesLanguage {
                engine,
                config,
                language,
                kind,
            } => {
                let engine = self.engine(&engine, Some(&language))?;
                let cfg = self.config(config)?;
                let language = language_from_id(&language);
                let provides = guarded(engine.name(), || {
                    Ok(match kind {
                        WireKind::Lint => engine.provides_language_lint(&language, &cfg),
                        WireKind::Format => engine.provides_language_format(&language, &cfg),
                    })
                })?;
                Ok(Payload::Bool(provides))
            }
            Request::BatchSupport { engine, config } => {
                let engine = self.engine(&engine, None)?;
                let cfg = self.config(config)?;
                let support = guarded(engine.name(), || Ok(engine.batch_support(&cfg)))?;
                Ok(Payload::BatchSupport(WireBatchSupport {
                    format: support.format,
                    lint: support.lint,
                }))
            }
            Request::DerivedOptionKeys { engine, table, options } => {
                let engine = self.engine(&engine, None)?;
                let options = match to_toml(&options) {
                    Ok(toml::Value::Table(table)) => table,
                    Ok(_) => return Err(protocol("options must be a table")),
                    Err(error) => return Err(protocol(format!("invalid options: {error}"))),
                };
                Ok(Payload::OptionKeys(derived_keys(engine, table, &options)?))
            }
            Request::RunContext { engine, config } => {
                let engine = self.engine(&engine, None)?;
                let cfg = self.config(config)?;
                let context = guarded(engine.name(), || Ok(engine.run_context(&cfg)))?;
                Ok(Payload::Text(context))
            }
            Request::FileContext { engine, config, path } => {
                let engine = self.engine(&engine, None)?;
                let cfg = self.config(config)?;
                let context = guarded(engine.name(), || Ok(engine.file_context(Path::new(&path), &cfg)))?;
                Ok(Payload::OptString(context))
            }
            Request::LintInputSalt { engine, config, path } => {
                let engine = self.engine(&engine, None)?;
                let cfg = self.config(config)?;
                let salt = guarded(engine.name(), || Ok(engine.lint_input_salt(Path::new(&path), &cfg)))?;
                Ok(Payload::OptString(salt))
            }
            Request::Rules { dirs, config, test } => {
                let handler = self.rules.ok_or_else(|| WireError {
                    kind: WireErrorKind::Unsupported,
                    message: "this component hosts no rule engine".to_owned(),
                    chain: Vec::new(),
                })?;
                let cfg = engine_config_from_wire(&config)?;
                let report = guarded("rules", || handler(&dirs, &cfg, test))?;
                Ok(Payload::Rules(report))
            }
            Request::FormatInputSalt { engine, config, path } => {
                let engine = self.engine(&engine, None)?;
                let cfg = self.config(config)?;
                let salt = guarded(engine.name(), || Ok(engine.format_input_salt(Path::new(&path), &cfg)))?;
                Ok(Payload::OptString(salt))
            }
        }
    }

    fn init(
        &self,
        host_protocol: u32,
        host_version: String,
        threads: u32,
        stack_size: u64,
        max_log_level: poly_component_protocol::WireLogLevel,
    ) -> Result<Payload, WireError> {
        if host_protocol != PROTOCOL_VERSION {
            return Err(protocol(format!(
                "the host speaks protocol {host_protocol}, this component speaks {PROTOCOL_VERSION}"
            )));
        }
        self.init
            .set(InitState {
                host_version,
                threads,
                stack_size,
                max_log_level,
            })
            .map_err(|_| protocol("Init was already called"))?;
        Ok(Payload::Init {
            protocol: PROTOCOL_VERSION,
            component_version: self.component_version.clone(),
        })
    }
}

/// What the engine's derived probe for `table` recognises in `options`; `None` when the table has
/// no probe or the probe could not tell.
fn derived_keys(
    engine: &dyn Engine,
    table: WireOptionTable,
    options: &toml::Table,
) -> Result<Option<Vec<String>>, WireError> {
    let local = TABLES
        .iter()
        .find(|(_, wire)| *wire == table)
        .map(|(local, _)| *local)
        .ok_or_else(|| protocol("unknown option table"))?;
    let probe = engine.option_keys(local).derived_probe();
    guarded(engine.name(), || Ok(probe.and_then(|probe| probe.recognized(options))))
}
