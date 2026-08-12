//! `poly config` — manage shared configuration: lock remote `extends` bases
//! (`poly config update`) and inspect the effective, fully-resolved config
//! (`poly config show`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use poly_config::PolyConfig;

use crate::config_sources::{self, RemoteExtendsResolver};

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
    /// Load the fully-resolved effective config (fetching pinned remote bases)
    /// and print a concise summary.
    #[command(alias = "resolve")]
    Show {
        /// Config file to resolve (defaults to the discovered `poly.toml`).
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
}

/// Run `poly config`, mapping any error to exit code 2.
pub fn run_config(args: ConfigArgs) -> ExitCode {
    let result = match args.command {
        ConfigCommand::Update { config } => update(config.as_deref()),
        ConfigCommand::Show { config } => show(config.as_deref()),
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

fn show(explicit: Option<&Path>) -> Result<ExitCode> {
    let root = config_sources::repo_root()?;
    let resolver = RemoteExtendsResolver::new(&root)?;
    let (config, config_path) = match explicit {
        Some(path) => (PolyConfig::load_file_with(path, &resolver)?, path.to_path_buf()),
        None => {
            let cwd = std::env::current_dir().context("resolving the working directory")?;
            (
                PolyConfig::load_with(&cwd, &resolver)?,
                config_sources::root_config_path(&root),
            )
        }
    };
    print_summary(&config, &config_path, &resolver);
    Ok(ExitCode::SUCCESS)
}

fn print_summary(config: &PolyConfig, config_path: &Path, resolver: &RemoteExtendsResolver) {
    let defaults = &config.defaults;
    println!("[defaults]");
    println!("    line_length            = {}", defaults.line_length);
    println!("    line_ending            = {:?}", defaults.line_ending);
    println!("    final_newline          = {}", defaults.final_newline);
    println!("    trim_trailing_whitespace = {}", defaults.trim_trailing_whitespace);

    // The effective exclude list is worth printing in full: it accumulates across
    // `extends` bases and `poly.local.toml`, so no single file shows it.
    print!("[discovery]  exclude = ");
    if config.discovery.exclude.is_empty() {
        println!("(none)");
    } else {
        let globs: Vec<&str> = config.discovery.exclude.iter().map(String::as_str).collect();
        println!("{}", globs.join(", "));
    }

    print_section_keys("lint", &config.lint);
    print_section_keys("fmt", &config.fmt);

    print!("[tools]  ");
    if config.tools.is_empty() {
        println!("(none)");
    } else {
        let names: Vec<&str> = config.tools.iter().map(|(name, _)| name.as_str()).collect();
        println!("{}", names.join(", "));
    }

    println!("[hooks]  present = {}", config.hooks.present);

    print_extends(config_path, resolver);
}

fn print_section_keys(label: &str, table: &toml::Table) {
    print!("[{label}]  ");
    if table.is_empty() {
        println!("(none)");
    } else {
        let keys: Vec<&str> = table.keys().map(String::as_str).collect();
        println!("{}", keys.join(", "));
    }
}

fn print_extends(config_path: &Path, resolver: &RemoteExtendsResolver) {
    let sources = match config_sources::declared_extends(config_path) {
        Ok(sources) => sources,
        Err(error) => {
            println!("[extends]  <unavailable: {error}>");
            return;
        }
    };
    if sources.is_empty() {
        println!("[extends]  (none)");
        return;
    }
    println!("[extends]");
    for source in &sources {
        if source.git.is_some() {
            let oid = resolver
                .resolved_oid(source)
                .unwrap_or_else(|| "<unlocked>".to_string());
            println!("    git  {} -> {oid}", source.display_id());
        } else {
            println!("    path {}", source.display_id());
        }
    }
}
