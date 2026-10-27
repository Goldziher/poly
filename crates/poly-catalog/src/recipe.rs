//! Execution recipes: how poly runs a catalog tool beyond "one spawn per file,
//! exit code is the verdict".
//!
//! A [`Recipe`] is poly-owned data layered over the vendored mdsf catalog (see
//! `data/overlay.json`). It answers the questions the mdsf argv cannot:
//!
//! - **What does the tool print?** ([`Output`]) — the parser that turns its
//!   report into file/line/column/rule diagnostics.
//! - **What is the unit of work?** ([`Group`] and [`Target`]) — one file, a set
//!   of files, a package directory, or a project root found by a marker file.
//! - **Which findings belong to the files being checked?** ([`Findings`]).
//! - **What else decides the answer?** (`dependencies`, `config_files`,
//!   `config_flags`) — folded into the result-cache key so an edit to a
//!   neighbouring file or a config file cannot serve a stale verdict.
//! - **Where does the tool come from?** ([`Install`]) — a pinned download.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The report format a tool prints, i.e. which parser reads it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    /// No structured report: the exit code is the verdict and a failing run
    /// yields one file-level finding carrying a snippet of its output.
    #[default]
    Exit,
    /// Checkstyle `-f xml`.
    CheckstyleXml,
    /// actionlint `-format '{{json .}}'`.
    ActionlintJson,
    /// buf `--error-format=json` (one JSON object per line).
    BufJson,
    /// hadolint `-f json`.
    HadolintJson,
    /// golangci-lint v2 `--output.json.path=stdout`.
    GolangciJson,
    /// buildifier `-format=json`.
    BuildifierJson,
    /// tflint `--format=json`.
    TflintJson,
    /// npm-groovy-lint `--output json`.
    GroovyJson,
    /// `helm lint` / `helm template` text.
    HelmText,
    /// `path:line:col: message` lines (gcc style), e.g. `terraform validate`.
    PathLine,
}

/// How a run's files are split into invocations.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    /// No grouping: every file of the run goes to the same invocation(s), run
    /// from poly's working directory. Invocations are sharded to stay inside
    /// the argv budget.
    #[default]
    None,
    /// One invocation per directory containing a file of the run.
    Dir,
    /// One invocation per project root: the nearest ancestor directory holding
    /// any of these marker files (`Chart.yaml`, `go.mod`, `buf.yaml`, ...).
    Marker(Vec<String>),
}

/// What an invocation is pointed at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// The run's files themselves (`$FILES`).
    #[default]
    Files,
    /// The project root of the group (`$ROOT`, relative to the invocation's
    /// working directory, so `.`).
    Root,
    /// The directory of each file (`$DIR`, relative to the group root), so a
    /// package-scoped tool sees whole packages and never a lone file.
    Dir,
}

/// Which findings of a grouped invocation are reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Findings {
    /// Only findings in files that are part of the run. Right for a linter that
    /// reports on a whole package or module when pointed at one file.
    #[default]
    Own,
    /// Every finding of the invocation. Right when a change in one file breaks
    /// another (a chart whose `values.yaml` no longer satisfies a template):
    /// findings in files outside the run are attached to the run's files with
    /// the real path in the message.
    All,
}

/// Where a tool's binary comes from when it is not on `PATH`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Install {
    /// Shell-free, human-readable instruction shown when the tool is absent
    /// (`brew install buf`).
    pub hint: String,
    /// Pinned releases poly can download itself, keyed by version.
    #[serde(default)]
    pub releases: BTreeMap<String, Release>,
}

/// One pinned, checksummed release of a tool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Release {
    /// How the download is unpacked.
    pub kind: ReleaseKind,
    /// One asset per `<os>-<arch>` platform (`darwin-arm64`, `linux-x86_64`);
    /// the key `any` serves every platform (a platform-independent jar).
    pub assets: BTreeMap<String, Asset>,
}

/// The packaging of a [`Release`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseKind {
    /// A standalone executable.
    Binary,
    /// A jar run as `java -jar`.
    Jar,
}

/// A downloadable file and the digest it must have.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    /// Download URL.
    pub url: String,
    /// Lower-case hex SHA-256 of the downloaded file.
    pub sha256: String,
}

/// Execution recipe for one catalog tool.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Recipe {
    /// Report format; selects the parser.
    pub output: Output,
    /// How files are split into invocations.
    pub group: Group,
    /// What each invocation is pointed at.
    pub target: Target,
    /// Which findings of an invocation are reported.
    pub findings: Findings,
    /// Globs, relative to the group root, of files whose content decides the
    /// verdict beyond the checked file itself (every `.proto` of a buf module,
    /// the `.go` files of a module). Their digest is folded into the cache key
    /// of every file of the group. Empty for tools that judge a file alone.
    pub dependencies: Vec<String>,
    /// Config file names the tool discovers on its own, searched upward from
    /// poly's working directory and fingerprinted into the cache key.
    pub config_files: Vec<String>,
    /// Argv flags whose following value names a config file (`-c`, `-p`); the
    /// named files are fingerprinted into the cache key.
    pub config_flags: Vec<String>,
    /// Name of the command that applies the tool's own fixes in place.
    pub fix: Option<String>,
    /// The tool is a JVM jar launched as `java -jar <jar>`.
    pub java: bool,
    /// Never run two invocations of this tool at once (a tool holding a global
    /// lock, or writing a shared cache).
    pub serial: bool,
    /// Fewest files worth a separate invocation. JVM tools pay a start-up that
    /// dwarfs the per-file work, so splitting a small run across cores loses.
    pub shard_floor: Option<usize>,
    /// Exit codes, besides 0, that mean the tool ran to completion (it found
    /// problems, or it changed a file) rather than failed.
    pub ok_exits: Vec<i32>,
    /// Absence handling and pinned downloads.
    pub install: Option<Install>,
}

impl Recipe {
    /// Whether files are handed to the tool in groups rather than as one flat
    /// list.
    pub fn is_grouped(&self) -> bool {
        self.group != Group::None
    }
}
