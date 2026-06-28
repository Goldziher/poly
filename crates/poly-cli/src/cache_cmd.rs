//! `poly cache` — maintenance commands over the tier-1 result cache.
//!
//! Subcommands operate on the cache at `.polylint/cache` (or an explicit
//! `--cache-dir`):
//!
//! - `poly cache stats` — entry counts, sizes, and format version (default).
//! - `poly cache size` — the total on-disk size in bytes.
//! - `poly cache gc [--max-age <days>] [--max-size <500M|2G|…>]` — evict stale
//!   and/or oversized entries (and wipe a tree from an incompatible layout).
//! - `poly cache clean` — remove every cached entry.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use poly_cache::{CacheStats, ResultCache, root_from_cwd};
use poly_config::PolyConfig;

const SECONDS_PER_DAY: u64 = 86_400;

/// `poly cache` arguments — an optional subcommand (defaulting to `stats`).
#[derive(Args)]
pub struct CacheArgs {
    /// The maintenance operation to perform (default: `stats`).
    #[command(subcommand)]
    pub command: Option<CacheCommand>,

    /// Operate on this cache directory instead of the resolved repo cache.
    #[arg(long)]
    pub cache_dir: Option<PathBuf>,
}

/// The `poly cache` subcommands.
#[derive(Subcommand)]
pub enum CacheCommand {
    /// Show entry counts, sizes, and the cache format version.
    Stats,
    /// Print the total on-disk size of all cached entries (bytes).
    Size,
    /// Evict stale or oversized entries (and wipe an incompatible-layout tree).
    Gc {
        /// Evict entries older than this many days.
        #[arg(long)]
        max_age: Option<u64>,
        /// Evict oldest-first until under this budget (e.g. `500M`, `2G`).
        #[arg(long)]
        max_size: Option<String>,
    },
    /// Remove every cached entry, keeping the directory layout.
    Clean,
}

/// Run `poly cache`, mapping any error to exit code 2.
pub fn run_cache(args: CacheArgs) -> ExitCode {
    match run(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("poly cache: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run(args: CacheArgs) -> Result<ExitCode> {
    let root = resolve_root(args.cache_dir.as_deref())?;
    let cache = ResultCache::open(root, true).context("failed to open the result cache")?;

    match args.command.unwrap_or(CacheCommand::Stats) {
        CacheCommand::Stats => print_stats(&cache.stats()?),
        CacheCommand::Size => println!("{}", cache.total_size()?),
        CacheCommand::Gc { max_age, max_size } => {
            let max_age = max_age.map(|days| Duration::from_secs(days * SECONDS_PER_DAY));
            let max_size = max_size.as_deref().map(parse_size).transpose()?;
            let freed = cache.gc(max_age, max_size)?;
            println!("freed {} ({} bytes)", format_iec(freed), freed);
        }
        CacheCommand::Clean => {
            let freed = cache.clean()?;
            println!("cleaned {} ({} bytes)", format_iec(freed), freed);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Resolve the cache root: `--cache-dir` if given, else `[cache] dir` from the
/// nearest config, else the default anchor walk from the current directory.
fn resolve_root(cache_dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = cache_dir {
        return Ok(dir.to_path_buf());
    }
    let cwd = std::env::current_dir().context("failed to resolve the working directory")?;
    let config = PolyConfig::load(&cwd).context("failed to load config")?;
    match config.cache.dir {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => root_from_cwd().context("failed to resolve the cache directory"),
    }
}

/// Render a [`CacheStats`] as a human-readable summary.
fn print_stats(stats: &CacheStats) {
    println!("cache format version: {}", stats.format_version);
    match &stats.on_disk_version {
        Some(version) if version == &stats.format_version => {
            println!("on-disk version:      {version}");
        }
        Some(version) => println!("on-disk version:      {version} (stale; gc will wipe)"),
        None => println!("on-disk version:      (none)"),
    }
    for namespace in &stats.per_namespace {
        println!(
            "  {:<5} {:>6} entries  {:>10}",
            namespace.namespace.as_dir(),
            namespace.entries,
            format_iec(namespace.bytes),
        );
    }
    println!(
        "total: {} entries, {} ({} bytes)",
        stats.per_namespace.iter().map(|n| n.entries).sum::<u64>(),
        format_iec(stats.total_bytes),
        stats.total_bytes,
    );
}

/// Parse a human size string (`512`, `64K`, `500M`, `2G`, `1T`) into bytes.
///
/// A bare number is bytes; a `K`/`M`/`G`/`T` suffix (case-insensitive) scales by
/// the corresponding power of 1024.
fn parse_size(value: &str) -> Result<u64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("empty size value");
    }
    let (number, scale) = match trimmed.as_bytes()[trimmed.len() - 1] {
        b'k' | b'K' => (&trimmed[..trimmed.len() - 1], 1024u64),
        b'm' | b'M' => (&trimmed[..trimmed.len() - 1], 1024 * 1024),
        b'g' | b'G' => (&trimmed[..trimmed.len() - 1], 1024 * 1024 * 1024),
        b't' | b'T' => (&trimmed[..trimmed.len() - 1], 1024u64.pow(4)),
        _ => (trimmed, 1u64),
    };
    let number: u64 = number
        .trim()
        .parse()
        .with_context(|| format!("invalid size `{value}` (expected e.g. `500M`, `2G`)"))?;
    number
        .checked_mul(scale)
        .with_context(|| format!("size `{value}` overflows a 64-bit byte count"))
}

/// Format a byte count using IEC binary units (`B`, `KiB`, `MiB`, …).
fn format_iec(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::{format_iec, parse_size};

    #[test]
    fn parse_size_handles_suffixes_and_bare_bytes() {
        assert_eq!(parse_size("512").unwrap(), 512);
        assert_eq!(parse_size("64K").unwrap(), 64 * 1024);
        assert_eq!(parse_size("500M").unwrap(), 500 * 1024 * 1024);
        assert_eq!(parse_size("2g").unwrap(), 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_size_rejects_garbage() {
        assert!(parse_size("").is_err());
        assert!(parse_size("abc").is_err());
        assert!(parse_size("12X").is_err());
    }

    #[test]
    fn format_iec_uses_binary_units() {
        assert_eq!(format_iec(512), "512 B");
        assert_eq!(format_iec(1024), "1.0 KiB");
        assert_eq!(format_iec(1536), "1.5 KiB");
        assert_eq!(format_iec(1024 * 1024), "1.0 MiB");
    }
}
