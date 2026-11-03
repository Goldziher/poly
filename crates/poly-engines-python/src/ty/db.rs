//! The salsa database a ty check runs on.
//!
//! The published ty crates stop short of `ty_project`, so the database `ty_project` would supply is
//! ported here at its minimum: a file registry, the [`OverlaySystem`], the vendored typeshed, one
//! fixed [`ProgramSettings`] and the default lint registry. There is no file watching, no
//! incremental revision handling and no project-level rule overrides, because a project check is a
//! one-shot function of its request.

// `#[salsa::db]` expands to an `unsafe impl` of salsa's storage trait; nothing here writes `unsafe`.
#![allow(unsafe_code)]

use std::sync::Arc;

use ruff_db::Db as SourceDb;
use ruff_db::diagnostic::Diagnostic;
use ruff_db::files::{File, FileRootKind, Files};
use ruff_db::system::{System, SystemPath};
use ruff_db::vendored::VendoredFileSystem;
use ty_module_resolver::Db as ModuleResolverDb;
use ty_python_core::program::ProgramSettings;
use ty_python_core::{Db as PythonCoreDb, ProgramFile, TestProgramDb};
use ty_python_semantic::dependency::DependencyMetadata;
use ty_python_semantic::lint::{LintRegistry, RuleSelection};
use ty_python_semantic::{AnalysisSettings, Db, PythonVersionWithSource, check_file_unwrap, default_lint_registry};

use super::system::OverlaySystem;

/// The database of one project check.
#[salsa::db]
#[derive(Clone)]
pub(super) struct TyDb {
    storage: salsa::Storage<Self>,
    files: Files,
    system: OverlaySystem,
    vendored: VendoredFileSystem,
    rule_selection: Arc<RuleSelection>,
    analysis_settings: Arc<AnalysisSettings>,
    program_settings: ProgramSettings,
}

impl TyDb {
    /// A database over `system` with ty's default rule selection and empty program settings.
    pub(super) fn new(system: OverlaySystem) -> Self {
        let vendored = ty_vendored::file_system().clone();
        let program_settings = ProgramSettings::empty(&vendored);
        Self {
            storage: salsa::Storage::new(None),
            files: Files::default(),
            system,
            vendored,
            rule_selection: Arc::new(RuleSelection::from_registry(default_lint_registry())),
            analysis_settings: Arc::new(AnalysisSettings::default()),
            program_settings,
        }
    }

    /// Install the program settings and register the file roots they and the project imply.
    ///
    /// Must run before any query, as ty's own `TestDb` does: roots are untracked state.
    pub(super) fn configure(&mut self, settings: ProgramSettings, project_root: &SystemPath) {
        let db: &Self = self;
        db.files().try_add_root(db, project_root, FileRootKind::Project);
        settings.search_paths.try_register_static_roots(db);
        self.program_settings = settings;
    }
}

#[salsa::db]
impl SourceDb for TyDb {
    fn vendored(&self) -> &VendoredFileSystem {
        &self.vendored
    }

    fn system(&self) -> &dyn System {
        &self.system
    }

    fn files(&self) -> &Files {
        &self.files
    }
}

#[salsa::db]
impl ModuleResolverDb for TyDb {}

#[salsa::db]
impl PythonCoreDb for TyDb {
    fn should_check_file(&self, file: File) -> bool {
        !file.path(self).is_vendored_path()
    }
}

#[salsa::db]
impl TestProgramDb for TyDb {
    fn program_settings(&self) -> &ProgramSettings {
        &self.program_settings
    }
}

#[salsa::db]
impl Db for TyDb {
    fn check_file(&self, file: File) -> Vec<Diagnostic> {
        if !self.should_check_file(file) {
            return Vec::new();
        }
        check_file_unwrap(self, self.program_file(file))
    }

    fn program_file(&self, file: File) -> ProgramFile<'_> {
        self.program().program_file(self, file)
    }

    fn python_version_with_source(&self, _file: File) -> &PythonVersionWithSource {
        &self.program_settings.python_version
    }

    fn rule_selection(&self, _file: File) -> &RuleSelection {
        &self.rule_selection
    }

    fn lint_registry(&self) -> &LintRegistry {
        default_lint_registry()
    }

    fn analysis_settings(&self, _file: File) -> &AnalysisSettings {
        &self.analysis_settings
    }

    fn dependency_metadata(&self, _file: File) -> Option<&DependencyMetadata> {
        None
    }

    fn verbose(&self) -> bool {
        false
    }

    fn is_open_file(&self, _file: File) -> bool {
        false
    }

    fn dyn_clone(&self) -> Box<dyn Db> {
        Box::new(self.clone())
    }
}

#[salsa::db]
impl salsa::Database for TyDb {}
