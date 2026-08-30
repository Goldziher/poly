//! `poly config` — manage shared configuration: lock remote `extends` bases
//! (`poly config update`) and inspect the effective, fully-resolved config
//! (`poly config show`).

mod effective;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use poly_core::report::{render_json, render_toon};

use crate::config_sources::{self, RemoteExtendsResolver};

pub use effective::EffectiveConfig;

/// `poly config` argument surface.
#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Resolve symbolic `extends` git refs to pinned object IDs and write
    /// `poly-config.lock`.
    ///
    /// v1 locks only the top-level config's direct git bases; a base's own
    /// (transitive) `extends` are not resolved.
    Update {
        /// Config file to lock (defaults to the repo-root `poly.toml`).
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
    /// Print the effective, fully-merged config — every section and key poly
    /// resolved, after `extends` bases, the `poly.toml` cascade and
    /// `poly.local.toml` were applied.
    ///
    /// The default TOML output is a valid config document, so diffing it
    /// against your own `poly.toml` shows exactly what poly kept.
    #[command(alias = "resolve")]
    Show {
        /// Config file to resolve (defaults to the discovered `poly.toml`).
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,

        /// Output format (`pretty` is an alias for `toml`).
        #[arg(long, value_enum, default_value_t = ConfigFormat::Toml)]
        format: ConfigFormat,
    },
}

/// Output format for `poly config show`.
///
/// TOML is the default because the input is TOML: the printed document parses
/// as a `poly.toml`, which is what makes "diff what you wrote against what poly
/// kept" a one-liner. The machine formats wrap the same config in a `config`
/// key alongside the `resolution` block that TOML carries as comments.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConfigFormat {
    /// The effective config as a TOML document, with resolution notes as comments.
    #[value(alias = "pretty")]
    Toml,
    /// JSON: `{ "config": …, "resolution": … }`.
    Json,
    /// TOON (Token-Oriented Object Notation), same shape as JSON.
    Toon,
}

/// Run `poly config`, mapping any error to exit code 2.
pub fn run_config(args: ConfigArgs) -> ExitCode {
    let result = match args.command {
        ConfigCommand::Update { config } => update(config.as_deref()),
        ConfigCommand::Show { config, format } => show(config.as_deref(), format),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("poly config: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn update(explicit: Option<&Path>) -> Result<ExitCode> {
    let root = config_sources::repo_root()?;
    let config_path = explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(|| config_sources::root_config_path(&root));
    if !config_path.is_file() {
        bail!("config file not found: {}", config_path.display());
    }
    let lock = config_sources::update(&root, &config_path)?;
    let count = lock.source_count();
    println!(
        "Locked {count} remote config base{}.",
        if count == 1 { "" } else { "s" }
    );
    Ok(ExitCode::SUCCESS)
}

fn show(explicit: Option<&Path>, format: ConfigFormat) -> Result<ExitCode> {
    let root = config_sources::repo_root()?;
    let resolver = RemoteExtendsResolver::new(&root)?;
    let document = EffectiveConfig::resolve(explicit, &resolver)?;
    let rendered = match format {
        ConfigFormat::Toml => document.to_toml()?,
        ConfigFormat::Json => render_json(&document)?,
        ConfigFormat::Toon => render_toon(&document)?,
    };
    println!("{}", rendered.trim_end());
    Ok(ExitCode::SUCCESS)
}
