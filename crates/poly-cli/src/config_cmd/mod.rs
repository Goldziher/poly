//! `poly config` — inspect shared configuration: the effective, fully-resolved
//! config (`poly config show`) and the `poly.toml` JSON Schema. Locking remote
//! `extends` bases is `poly lock`.

mod effective;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result};
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
    /// Print the effective, fully-merged config — every section and key poly
    /// resolved, after `extends` bases, the `poly.toml` cascade and
    /// `poly.local.toml` were applied.
    ///
    /// The default TOML output is a valid config document, so diffing it
    /// against your own `poly.toml` shows exactly what poly kept.
    #[command(alias = "resolve")]
    Show {
        /// Output format (`pretty` is an alias for `toml`).
        #[arg(long, value_enum, default_value_t = ConfigFormat::Toml)]
        format: ConfigFormat,
    },
    /// Print the JSON Schema for `poly.toml`.
    ///
    /// Editors use it for validation and autocomplete — point them at it with a
    /// `#:schema <path-or-url>` first line in `poly.toml`, or the `taplo` schema
    /// association. The schema covers every typed section; the per-engine
    /// `[lint]` / `[fmt]` tables list each backend's declared keys.
    Schema {
        /// Write the schema to this file instead of stdout.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
    /// Check `poly.toml` against poly's static knowledge of every engine, without loading or
    /// downloading any component.
    ///
    /// Option keys are read from the compiled component index, so the verdict is the same
    /// whatever is installed. Exits 0 when valid and 2 when not. Components whose engines derive
    /// further keys at run time are listed as only partially validated.
    Validate {
        /// Config files, or directories holding a `poly.toml`. Defaults to the nearest `poly.toml`.
        #[arg(value_name = "PATHS")]
        paths: Vec<PathBuf>,
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
        ConfigCommand::Show { format } => show(crate::global().config.as_deref(), format),
        ConfigCommand::Schema { output } => schema(output.as_deref()),
        ConfigCommand::Validate { paths } => validate(&paths),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("poly config: {error:#}");
            ExitCode::from(2)
        }
    }
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

/// Print — or write — the `poly.toml` JSON Schema.
fn schema(output: Option<&Path>) -> Result<ExitCode> {
    let value = poly_core::config_schema::poly_toml_schema();
    let mut text = serde_json::to_string_pretty(&value).context("serializing the config schema")?;
    text.push('\n');
    match output {
        Some(path) => {
            std::fs::write(path, text).with_context(|| format!("writing the schema to {}", path.display()))?;
            println!("Wrote the poly.toml schema to {}.", path.display());
        }
        None => print!("{text}"),
    }
    Ok(ExitCode::SUCCESS)
}

/// Validate each config file; exit 0 when all are valid, 2 when any is not.
fn validate(paths: &[PathBuf]) -> Result<ExitCode> {
    let files = if paths.is_empty() {
        let cwd = std::env::current_dir().context("reading the current directory")?;
        vec![poly_config::find_config(&cwd).context("no poly.toml found")?]
    } else {
        paths
            .iter()
            .map(|path| {
                if path.is_dir() {
                    path.join(poly_config::CONFIG_FILE_NAMES[0])
                } else {
                    path.clone()
                }
            })
            .collect()
    };
    let mut valid = true;
    let mut partial: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> = Default::default();
    for file in &files {
        let source = std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let report = poly_core::config_validate::validate_source(&source);
        for problem in &report.problems {
            let at = if problem.path.is_empty() {
                String::new()
            } else {
                format!(" [{}]", problem.path)
            };
            println!("{}{at}: {}", file.display(), problem.message);
        }
        valid &= report.is_valid();
        for entry in report.partial {
            partial.entry(entry.component).or_default().extend(entry.engines);
        }
    }
    for (component, engines) in &partial {
        let engines = engines.iter().cloned().collect::<Vec<_>>().join(", ");
        println!("partially validated: component `{component}` (engines: {engines}); derived keys could not be probed");
    }
    if valid {
        println!("config is valid ({} checked)", files.len());
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(2))
    }
}
