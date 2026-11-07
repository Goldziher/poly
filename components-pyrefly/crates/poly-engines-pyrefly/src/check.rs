//! Python type checking with pyrefly, as a project-check engine.
//!
//! pyrefly is consumed as a git dependency pinned to a release tag (see `components-pyrefly/Cargo.toml`;
//! the crates.io `pyrefly` is not the type checker). It is driven through its `State` and `Transaction`
//! with an in-memory overlay of the files the host handed over, so findings are computed on those
//! contents, not whatever is on disk. The pinned tag is folded into [`PyreflyEngine::version`], so
//! bumping it invalidates cached results.
//!
//! # Configuration
//!
//! The engine is opt-in and shares its table with the ty backend:
//!
//! ```toml
//! [engines.python-types]
//! backend = "pyrefly"
//! python_version = "3.12"      # default: the interpreter's, else pyrefly's latest
//! src = ["src"]                # first-party roots; default: the project root and `src/`
//! extra_paths = ["stubs"]      # searched before first-party code
//! ```
//!
//! Third-party imports resolve through the `site-packages` directories of the interpreter the host
//! resolved; nothing is executed. pyrefly's own `pyrefly.toml` and `[tool.pyrefly]` are not read.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use poly_engine_api::engine::{
    Capabilities, Diagnostic, Engine, OptionKeys, OptionTable, OptionType, ProjectFile, ProjectRequest, Severity, Span,
    WireSelection,
};
use poly_engine_api::language::Language;
use pyrefly::state::load::FileContents;
use pyrefly::state::require::Require;
use pyrefly::state::state::State;
use pyrefly_build::handle::Handle;
use pyrefly_config::error_kind::Severity as PyreflySeverity;
use pyrefly_config::finder::ConfigFinder;
use pyrefly_python::module_path::ModulePath;
use pyrefly_util::arc_id::ArcId;
use pyrefly_util::thread_pool::ThreadCount;

use crate::settings;

/// The engine id. Distinct from the `python-types` table that configures it: the engine index gives
/// every engine exactly one component, and ty's `python-types` lives in the `python` component.
pub const ENGINE_NAME: &str = "pyrefly";

/// The engine table this engine shares with ty, whose `backend` key picks between them.
const TABLE: &str = "python-types";

/// The `backend` value that switches this engine on.
const BACKEND: &str = "pyrefly";

/// pyrefly's inference recurses deeply; checking runs on a thread with this much stack.
const WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;

const OPTION_KEYS: &[(&str, OptionType)] = &[
    ("backend", OptionType::STRING),
    ("python_version", OptionType::STRING),
    ("src", OptionType::ARRAY),
    ("extra_paths", OptionType::ARRAY),
];

/// The pyrefly-backed Python type checker.
#[derive(Debug, Default, Clone, Copy)]
pub struct PyreflyEngine;

impl Engine for PyreflyEngine {
    fn name(&self) -> &'static str {
        ENGINE_NAME
    }

    fn languages(&self) -> &'static [Language] {
        &[Language::Python]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: false,
            format: false,
            fix: false,
            project: true,
        }
    }

    fn option_keys(&self, table: OptionTable) -> OptionKeys {
        match table {
            OptionTable::Lint => OptionKeys::declared(OPTION_KEYS),
            OptionTable::Format => OptionKeys::declared(&[]),
            OptionTable::CrossCuttingLint => OptionKeys::UNCHECKED,
        }
    }

    fn version(&self) -> &str {
        "pyrefly-1.4.1+proj1"
    }

    fn selection(&self) -> Option<WireSelection> {
        Some(WireSelection {
            table: TABLE.to_owned(),
            key: "backend".to_owned(),
            value: BACKEND.to_owned(),
            default: false,
        })
    }

    fn project_check(&self, req: &ProjectRequest<'_>) -> anyhow::Result<Vec<(PathBuf, Vec<Diagnostic>)>> {
        // The host plans this engine only for its own backend (`Engine::selection`); this is a guard for a direct call.
        if req.config.options.get("backend").and_then(toml::Value::as_str) != Some(BACKEND) {
            return Ok(Vec::new());
        }
        let files: Vec<&ProjectFile> = req
            .files
            .iter()
            .filter(|file| file.language == Language::Python)
            .collect();
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let outcome = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .name("poly-pyrefly".to_string())
                .stack_size(WORKER_STACK_SIZE)
                .spawn_scoped(scope, || check(req, &files))
                .context("cannot start the pyrefly worker thread")
                .map(std::thread::ScopedJoinHandle::join)
        })?;
        outcome.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }
}

fn severity(severity: PyreflySeverity) -> Option<Severity> {
    match severity {
        PyreflySeverity::Error => Some(Severity::Error),
        PyreflySeverity::Warn => Some(Severity::Warning),
        PyreflySeverity::Info => Some(Severity::Info),
        PyreflySeverity::Ignore => None,
    }
}

/// Type-check `files` with one state, reporting under each file's path as the host gave it.
fn check(req: &ProjectRequest<'_>, files: &[&ProjectFile]) -> anyhow::Result<Vec<(PathBuf, Vec<Diagnostic>)>> {
    let root = settings::absolute(req.root)?;
    let config = settings::config(req, &root)?;
    let config = ArcId::new(config);
    let config_finder = ConfigFinder::new_constant(config.clone());

    let mut originals: BTreeMap<PathBuf, &PathBuf> = BTreeMap::new();
    let mut memory = Vec::with_capacity(files.len());
    for file in files {
        let path = settings::absolute(&file.path)?;
        memory.push((
            path.clone(),
            Some(Arc::new(FileContents::from_source(file.content.to_string()))),
        ));
        originals.insert(path, &file.path);
    }

    let state = State::new(config_finder, ThreadCount::AllThreads);
    // In-memory handles: a file-system handle is read from disk, never from the overlay.
    let handles: Vec<Handle> = originals
        .keys()
        .map(|path| config.handle_from_module_path(ModulePath::memory(path.clone())))
        .collect();
    let mut transaction = state.new_transaction(Require::Exports, None);
    transaction.set_memory(memory);
    transaction.run(&handles, Require::Errors, None);

    let mut by_file: BTreeMap<PathBuf, Vec<Diagnostic>> = BTreeMap::new();
    for error in transaction.get_errors(handles.iter()).collect_errors().ordinary {
        let Some(severity) = severity(error.severity()) else {
            continue;
        };
        let Some(original) = originals.get(error.path().as_path()) else {
            tracing::debug!(
                code = error.error_kind().to_name(),
                "pyrefly: diagnostic outside the project"
            );
            continue;
        };
        let range = error.display_range();
        let description = error.msg_details().filter(|text| !text.is_empty()).map(str::to_owned);
        by_file.entry((*original).clone()).or_default().push(Diagnostic {
            engine: ENGINE_NAME.to_string(),
            code: Some(error.error_kind().to_name().to_owned()),
            severity,
            title: error.msg_header().to_owned(),
            description,
            span: Some(Span {
                start_line: range.start.line_within_file().get(),
                start_col: range.start.column().get(),
                end_line: range.end.line_within_file().get(),
                end_col: range.end.column().get(),
            }),
            url: None,
            fix: Vec::new(),
            metadata: BTreeMap::new(),
        });
    }
    Ok(by_file.into_iter().collect())
}
