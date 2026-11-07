//! The inputs of [`Engine::project_check`](crate::engine::Engine::project_check): the file set,
//! the config and the host-resolved environment.

use std::path::PathBuf;
use std::sync::Arc;

use crate::config::EngineConfig;
use crate::language::Language;

/// One file handed to [`Engine::project_check`](crate::engine::Engine::project_check).
#[derive(Debug, Clone)]
pub struct ProjectFile {
    /// Path to the file on disk, as discovered (the key the findings are reported under).
    pub path: PathBuf,
    /// Detected language of the file.
    pub language: Language,
    /// Full file contents, shared rather than copied.
    pub content: Arc<str>,
}

/// A Python interpreter and the import roots it exposes, resolved by the host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PythonEnvironment {
    /// The interpreter executable (a venv's `bin/python`, or `python3` from `PATH`).
    pub interpreter: PathBuf,
    /// Existing `site-packages` directories of the interpreter's environment.
    pub site_packages: Vec<PathBuf>,
    /// The interpreter's version (`3.12.4`) when it could be read without running it.
    pub version: Option<String>,
}

/// The toolchain environment a project check may see, resolved **by the host**.
///
/// A component never spawns a process or probes `PATH`: anything a check needs to know about the
/// machine is resolved by the host before the call and passed in here, so a check is a pure
/// function of `(files, config, environment)` and its result can be cached on those inputs.
/// Extensible: new toolchains add fields, and every field defaults to "not found".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectEnvironment {
    /// The project's Python environment, when one was found.
    pub python: Option<PythonEnvironment>,
}

impl ProjectEnvironment {
    /// The wire form (`Request::ProjectCheck::environment`): flat `(key, value)` pairs.
    ///
    /// Keys: `python.interpreter`, `python.site_packages` (paths joined by a newline),
    /// `python.version`. Pairs are emitted in a fixed order so equal environments serialize
    /// identically.
    #[must_use]
    pub fn to_wire_pairs(&self) -> Vec<(String, String)> {
        let Some(python) = &self.python else {
            return Vec::new();
        };
        let mut pairs = vec![(
            "python.interpreter".to_string(),
            python.interpreter.display().to_string(),
        )];
        let sites: Vec<String> = python.site_packages.iter().map(|p| p.display().to_string()).collect();
        pairs.push(("python.site_packages".to_string(), sites.join("\n")));
        if let Some(version) = &python.version {
            pairs.push(("python.version".to_string(), version.clone()));
        }
        pairs
    }

    /// Rebuild an environment from [`ProjectEnvironment::to_wire_pairs`] output; unknown keys are ignored.
    #[must_use]
    pub fn from_wire_pairs(pairs: &[(String, String)]) -> Self {
        let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        let Some(interpreter) = get("python.interpreter") else {
            return Self::default();
        };
        Self {
            python: Some(PythonEnvironment {
                interpreter: PathBuf::from(interpreter),
                site_packages: get("python.site_packages")
                    .map(|text| text.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect())
                    .unwrap_or_default(),
                version: get("python.version").map(str::to_string),
            }),
        }
    }
}

/// Everything [`Engine::project_check`](crate::engine::Engine::project_check) sees.
#[derive(Debug, Clone, Copy)]
pub struct ProjectRequest<'a> {
    /// The project root: the directory of the config governing the files.
    pub root: &'a std::path::Path,
    /// The files in scope for this engine, with contents.
    pub files: &'a [ProjectFile],
    /// The engine's resolved config.
    pub config: &'a EngineConfig,
    /// The host-resolved toolchain environment.
    pub environment: &'a ProjectEnvironment,
}

/// The language id of a listing entry: a file under a declared directory that the engine does not
/// serve, handed over by name with empty content (see [`ProjectDirs::list_unserved`]).
pub const LISTING_LANGUAGE: &str = "listing";

/// A file a project check reads besides the files it serves, named by an engine option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSideFile {
    /// The option naming the path, relative to the project root.
    pub option_key: String,
    /// The path used when the option is unset.
    pub default_path: String,
}

/// The directories a project check is about, named by engine options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDirs {
    /// The option naming one directory, relative to the project root.
    pub dir_key: String,
    /// The directory used when the option is unset.
    pub dir_default: String,
    /// An option holding a list of directories that replaces `dir_key`, when the engine has one.
    pub paths_key: Option<String>,
    /// A boolean option that extends the directories to their subdirectories, when the engine has one.
    pub recursive_key: Option<String>,
    /// Whether the directories extend to their subdirectories when `recursive_key` is unset or the
    /// engine has none: a tree-wide check such as a JavaScript workspace is about the whole tree.
    pub recursive_default: bool,
    /// Whether the host also lists the files in these directories that the engine does not serve
    /// (every file type, names only, empty content, language `Other("listing")`).
    pub list_unserved: bool,
}

/// What a project-check engine needs from the host beyond the files of the languages it serves.
///
/// Declared by the engine, carried in its description (and so in the component index), and read
/// by the host: the core holds no table keyed by engine name. Everything here is resolved by the
/// host and handed over as content, because a component reads nothing itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectInputs {
    /// An option that must be set for the engine to be planned at all, so a project that never
    /// configured it pays nothing for the phase.
    pub gate_key: Option<String>,
    /// Side files, each loaded when it exists.
    pub files: Vec<ProjectSideFile>,
    /// When set, only served files under these directories are handed over.
    pub dirs: Option<ProjectDirs>,
}
