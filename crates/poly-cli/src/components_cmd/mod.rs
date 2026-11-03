//! `poly components` — inspect and manage the downloadable engine components.
//!
//! `list` and `status` never download (see [`ops`]); `install`, `sync` and `import` are the only
//! subcommands that put anything in the cache. On a build that links every engine family the
//! components are bundled, so `install` only prints a note.

mod ops;
mod prune;
mod render;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Args, Subcommand};
use poly_component_host::ComponentHost;
use poly_core::components::{component_is_linked, default_cache_root, host_options, needed_component_names};

use crate::OutputFormat;
use ops::Env;

pub use ops::{Actions, ComponentRow, DEFAULT_KEEP_DAYS, Listing, SCHEMA, State};

/// `poly components` arguments.
#[derive(Args)]
pub struct ComponentsArgs {
    /// What to do.
    #[command(subcommand)]
    command: ComponentsCommand,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Pretty, global = true)]
    format: OutputFormat,
}

#[derive(Subcommand)]
enum ComponentsCommand {
    /// Every component and whether it is bundled, installed or missing. Never downloads.
    List,
    /// The components the given paths need and whether they are usable. Never downloads.
    ///
    /// Exits 1 when a needed component is missing or fails `--verify`.
    Status {
        /// Files or directories to inspect (default: the current directory).
        paths: Vec<PathBuf>,
        /// Re-check each installed archive against the lock.
        #[arg(long)]
        verify: bool,
    },
    /// Download and verify components: names, presets or `all`.
    Install {
        /// Component names or presets.
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Remove downloaded components: names, presets or `all`.
    Uninstall {
        /// Component names or presets.
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Install every component the given paths need. With `--offline`, only checks (exit 2 if
    /// something is missing).
    Sync {
        /// Files or directories to inspect (default: the current directory).
        paths: Vec<PathBuf>,
    },
    /// Remove cached archives the lock no longer references.
    Prune {
        /// Remove every cached archive, referenced or not.
        #[arg(long)]
        all: bool,
        /// Report what would be removed and remove nothing.
        #[arg(long)]
        dry_run: bool,
        /// Keep unreferenced archives used within this many days.
        #[arg(long, default_value_t = DEFAULT_KEEP_DAYS, value_name = "DAYS")]
        keep_days: u64,
    },
    /// Install a component archive from disk, verified against the lock (for air-gapped hosts).
    Import {
        /// The archive.
        archive: PathBuf,
    },
}

/// `component` is accepted as an alias at the top level (see `main.rs`).
pub fn run_components(args: ComponentsArgs) -> ExitCode {
    match run(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("poly components: {error:#}");
            ExitCode::from(2)
        }
    }
}

/// Open the host over the lock a run would use. `offline` for everything but `install`, `sync`
/// and `import`, so the read-only commands cannot reach the network even by mistake.
fn open_host(offline: bool) -> anyhow::Result<(ComponentHost, String)> {
    let mut options = host_options()?;
    let lock = options.lock_json.clone();
    options.offline = offline;
    options.gate_loads = true;
    Ok((ComponentHost::new(options)?, lock))
}

fn cache_root_of(host: Option<&ComponentHost>) -> PathBuf {
    host.map_or_else(
        || poly_config::network::component_cache_dir().unwrap_or_else(default_cache_root),
        |host| host.manager().cache_root().to_path_buf(),
    )
}

fn needed(paths: &[PathBuf]) -> anyhow::Result<Vec<String>> {
    let paths = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.to_vec()
    };
    let resolver = crate::config_sources::resolver()?;
    let cwd = std::env::current_dir().context("failed to resolve the working directory")?;
    let config: poly_core::Config = match crate::global().config.as_deref() {
        Some(file) => poly_config::PolyConfig::load_file_with(file, &resolver)?,
        None => poly_config::PolyConfig::load_with(&cwd, &resolver)?,
    }
    .into();
    needed_component_names(poly_component_index::index(), &paths, &config, &resolver)
}

fn run(args: ComponentsArgs) -> anyhow::Result<ExitCode> {
    let format = args.format;
    let offline = crate::offline();
    let read_only = matches!(
        args.command,
        ComponentsCommand::List | ComponentsCommand::Status { .. } | ComponentsCommand::Prune { .. }
    );
    let (host, lock_json, no_host) = match open_host(offline || read_only) {
        Ok((host, lock)) => (Some(host), Some(lock), None),
        Err(error) => (None, None, Some(format!("{error:#}"))),
    };
    let env = Env {
        index: poly_component_index::index(),
        host: host.as_ref(),
        no_host,
        lock_json: lock_json.as_deref(),
        linked: &component_is_linked,
        offline,
        cache_root: cache_root_of(host.as_ref()),
    };
    match args.command {
        ComponentsCommand::List => {
            render::emit(format, &ops::list(&env), render::print_listing)?;
            Ok(ExitCode::SUCCESS)
        }
        ComponentsCommand::Status { paths, verify } => {
            let listing = ops::status(&env, &needed(&paths)?, verify);
            render::emit(format, &listing, render::print_listing)?;
            Ok(if listing.ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        ComponentsCommand::Install { names } => {
            let selected = ops::resolve(env.index, &names)?;
            finish(format, &ops::install(&env, &selected)?)
        }
        ComponentsCommand::Uninstall { names } => {
            let selected = ops::resolve(env.index, &names)?;
            finish(format, &ops::uninstall(&env, &selected)?)
        }
        ComponentsCommand::Sync { paths } => finish(format, &ops::sync(&env, &needed(&paths)?)?),
        ComponentsCommand::Prune {
            all,
            dry_run,
            keep_days,
        } => finish(format, &ops::prune(&env, all, dry_run, keep_days)?),
        ComponentsCommand::Import { archive } => finish(format, &ops::import(&env, &archive)?),
    }
}

fn finish(format: OutputFormat, actions: &Actions) -> anyhow::Result<ExitCode> {
    render::emit(format, actions, render::print_actions)?;
    Ok(if actions.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

/// Every component and where it stands, as the `components.list` host service reports it to a
/// frontend. Never downloads; a host that cannot be opened (no lock) lists each component as
/// unavailable with the reason.
#[must_use]
pub fn component_summaries() -> Vec<poly_frontend_api::dto::ComponentSummary> {
    let (host, no_host) = match open_host(true) {
        Ok((host, _)) => (Some(host), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let env = Env {
        index: poly_component_index::index(),
        host: host.as_ref(),
        no_host,
        lock_json: None,
        linked: &component_is_linked,
        offline: crate::offline(),
        cache_root: cache_root_of(host.as_ref()),
    };
    ops::list(&env)
        .components
        .into_iter()
        .map(|row| poly_frontend_api::dto::ComponentSummary {
            name: row.name,
            kind: row.kind.to_owned(),
            state: row.state.label().to_owned(),
            detail: row.detail,
        })
        .collect()
}

/// What `poly doctor` reports about components.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DoctorComponents {
    /// `bundled`, `core` or `partial`.
    pub flavor: &'static str,
    /// Where components are cached.
    pub cache_root: String,
    /// Whether the run is offline.
    pub offline: bool,
    /// Why no component lock is available, when none is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock: Option<String>,
    /// Components downloaded and cached.
    pub installed: Vec<String>,
    /// Components the current directory needs that are not usable.
    pub needed_missing: Vec<String>,
}

/// Collect the doctor's components section. Never downloads; a failure to work out what the
/// current directory needs leaves `needed_missing` empty.
#[must_use]
pub fn doctor_components() -> DoctorComponents {
    let offline = crate::offline();
    let (host, no_host) = match open_host(true) {
        Ok((host, _)) => (Some(host), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let env = Env {
        index: poly_component_index::index(),
        host: host.as_ref(),
        no_host,
        lock_json: None,
        linked: &component_is_linked,
        offline,
        cache_root: cache_root_of(host.as_ref()),
    };
    let listing = ops::list(&env);
    let installed = listing
        .components
        .iter()
        .filter(|c| c.state == State::Installed)
        .map(|c| c.name.clone())
        .collect();
    let needed_missing = needed(&[]).map_or_else(
        |_| Vec::new(),
        |names| ops::status(&env, &names, false).missing.unwrap_or_default(),
    );
    DoctorComponents {
        flavor: listing.flavor,
        cache_root: listing.cache_root,
        offline,
        lock: listing.lock,
        installed,
        needed_missing,
    }
}
