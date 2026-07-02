//! Conformance harness: a dev-only differential tester that measures how close
//! `poly fmt` is to each language's idiomatic reference formatter.
//!
//! Two subcommands:
//!
//! - `generate [--lang L]…` builds the pinned reference-tool Docker image
//!   (`docker/<lang>.Dockerfile`, tagged `conformance-<lang>`) and pipes every
//!   file under `corpus/<lang>/` through it (stdin → stdout) to produce the
//!   committed golden output under `golden/<lang>/`. Requires Docker.
//! - `check [--lang L]… [--min S]` runs poly fmt (via `polylint-core`) over the
//!   same corpus and scores its output against the golden — exact byte match
//!   plus a line-similarity ratio — so we can track per-language convergence.
//!   Hermetic: no Docker, only the committed golden files.
//!
//! The shipped `poly` binary never depends on this crate or on
//! any reference tool; this harness exists only to derive and validate the
//! conventions our pure-Rust formatters should follow.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use polylint_core::{Config, RunOptions};
use serde::Deserialize;

#[derive(Parser)]
#[command(about = "Differential conformance harness: poly fmt vs reference formatters")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand)]
enum CommandKind {
    /// Build reference-tool images and regenerate golden output (needs Docker).
    Generate {
        /// Restrict to these languages (default: all in tools.toml).
        #[arg(long = "lang")]
        languages: Vec<String>,
    },
    /// Score poly fmt against the committed golden output (hermetic).
    Check {
        /// Restrict to these languages (default: all in tools.toml).
        #[arg(long = "lang")]
        languages: Vec<String>,
        /// Fail if any language's mean similarity falls below this (0.0–1.0).
        #[arg(long)]
        min: Option<f64>,
    },
}

/// One language entry in `tools.toml`.
#[derive(Debug, Deserialize)]
struct LanguageSpec {
    reference: String,
    extensions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    languages: BTreeMap<String, LanguageSpec>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = manifest_root();
    let manifest = load_manifest(&root)?;

    match cli.command {
        CommandKind::Generate { languages } => generate(&root, &manifest, &languages),
        CommandKind::Check { languages, min } => check(&root, &manifest, &languages, min),
    }
}

/// The crate directory, which anchors `corpus/`, `golden/`, `docker/`, `tools.toml`.
fn manifest_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn load_manifest(root: &Path) -> Result<Manifest> {
    let path = root.join("tools.toml");
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading manifest {}", path.display()))?;
    toml::from_str(&text).context("parsing tools.toml")
}

/// Languages to act on: the explicit `--lang` list, or every language in the
/// manifest. Errors on an unknown language name.
fn selected<'a>(manifest: &'a Manifest, requested: &[String]) -> Result<Vec<&'a String>> {
    if requested.is_empty() {
        return Ok(manifest.languages.keys().collect());
    }
    for name in requested {
        if !manifest.languages.contains_key(name) {
            bail!("unknown language '{name}' (not in tools.toml)");
        }
    }
    Ok(manifest.languages.keys().filter(|k| requested.contains(k)).collect())
}

// ---------------------------------------------------------------------------
// generate
// ---------------------------------------------------------------------------

fn generate(root: &Path, manifest: &Manifest, requested: &[String]) -> Result<()> {
    let mut failures = Vec::new();
    for lang in selected(manifest, requested)? {
        if let Err(err) = generate_language(root, manifest, lang) {
            // One unavailable/broken reference tool must not block the others.
            eprintln!("  !! {lang}: {err:#}");
            failures.push(lang.clone());
        }
    }
    if !failures.is_empty() {
        eprintln!("\ngenerate completed with failures: {}", failures.join(", "));
    }
    Ok(())
}

fn generate_language(root: &Path, manifest: &Manifest, lang: &str) -> Result<()> {
    let spec = &manifest.languages[lang];
    println!("== {lang} (reference: {}) ==", spec.reference);
    build_image(root, lang)?;
    let corpus = corpus_files(root, lang, spec)?;
    if corpus.is_empty() {
        println!("  (no corpus files; skipping)");
        return Ok(());
    }
    let golden_dir = root.join("golden").join(lang);
    std::fs::create_dir_all(&golden_dir)?;
    for file in corpus {
        let input = std::fs::read(&file)?;
        let formatted =
            run_reference(lang, &input).with_context(|| format!("formatting {} via reference tool", file.display()))?;
        let name = file.file_name().unwrap();
        std::fs::write(golden_dir.join(name), &formatted)?;
        println!("  golden: {}", golden_dir.join(name).display());
    }
    Ok(())
}

/// Build the pinned reference image `conformance-<lang>` from its Dockerfile.
fn build_image(root: &Path, lang: &str) -> Result<()> {
    let dockerfile = root.join("docker").join(format!("{lang}.Dockerfile"));
    if !dockerfile.is_file() {
        bail!("missing {}", dockerfile.display());
    }
    let status = Command::new("docker")
        .args(["build", "-q", "-t"])
        .arg(format!("conformance-{lang}"))
        .arg("-f")
        .arg(&dockerfile)
        .arg(root.join("docker"))
        .status()
        .context("running docker build (is Docker installed and running?)")?;
    if !status.success() {
        bail!("docker build failed for {lang}");
    }
    Ok(())
}

/// Pipe `input` through the reference image (stdin → stdout) and return its
/// formatted output.
fn run_reference(lang: &str, input: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    let mut child = Command::new("docker")
        .args(["run", "--rm", "-i"])
        .arg(format!("conformance-{lang}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running docker run")?;
    child.stdin.take().context("docker stdin")?.write_all(input)?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "reference formatter exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out.stdout)
}

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

fn check(root: &Path, manifest: &Manifest, requested: &[String], min: Option<f64>) -> Result<()> {
    let mut failed = Vec::new();
    for lang in selected(manifest, requested)? {
        let spec = &manifest.languages[lang];
        let golden_dir = root.join("golden").join(lang);
        let corpus = corpus_files(root, lang, spec)?;
        let mut scored = 0usize;
        let mut exact = 0usize;
        let mut similarity_sum = 0.0;

        for file in &corpus {
            let golden_path = golden_dir.join(file.file_name().unwrap());
            let Ok(golden) = std::fs::read_to_string(&golden_path) else {
                continue; // no golden yet for this file
            };
            let ours = poly_fmt_output(file)?;
            scored += 1;
            if ours == golden {
                exact += 1;
            }
            similarity_sum += similarity(&ours, &golden);
        }

        if scored == 0 {
            println!("{lang:<8}  (no golden committed — run `conformance generate --lang {lang}`)");
            continue;
        }
        let mean = similarity_sum / scored as f64;
        println!(
            "{lang:<8}  exact {exact}/{scored}   mean similarity {:.1}%   (ref: {})",
            mean * 100.0,
            manifest.languages[lang].reference,
        );
        if let Some(threshold) = min
            && mean < threshold
        {
            failed.push(format!("{lang} mean {:.3} < {threshold:.3}", mean));
        }
    }

    if !failed.is_empty() {
        bail!("conformance below threshold:\n  {}", failed.join("\n  "));
    }
    Ok(())
}

/// Format one file the way `poly fmt` would, returning the resulting source.
fn poly_fmt_output(file: &Path) -> Result<String> {
    let original = std::fs::read_to_string(file)?;
    // Run the real pipeline in a temp dir so discovery/routing match production.
    let dir = tempfile::tempdir()?;
    let name = file.file_name().context("corpus file name")?;
    let target = dir.path().join(name);
    std::fs::write(&target, &original)?;

    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
    };
    let results = polylint_core::format(std::slice::from_ref(&target), &Config::default(), &opts, false, false)?;
    Ok(results
        .into_iter()
        .find(|r| r.path == target)
        .and_then(|r| r.formatted)
        .unwrap_or(original))
}

/// difflib-style similarity ratio over lines: `2·LCS / (len_a + len_b)`.
fn similarity(a: &str, b: &str) -> f64 {
    let al: Vec<&str> = a.lines().collect();
    let bl: Vec<&str> = b.lines().collect();
    if al.is_empty() && bl.is_empty() {
        return 1.0;
    }
    let lcs = lcs_len(&al, &bl);
    2.0 * lcs as f64 / (al.len() + bl.len()) as f64
}

/// Length of the longest common subsequence of two line slices (DP).
fn lcs_len(a: &[&str], b: &[&str]) -> usize {
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    for &ai in a {
        for (j, &bj) in b.iter().enumerate() {
            cur[j + 1] = if ai == bj { prev[j] + 1 } else { prev[j + 1].max(cur[j]) };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Files under `corpus/<lang>/`, sorted, whose extension is declared for the
/// language in `tools.toml`.
fn corpus_files(root: &Path, lang: &str, spec: &LanguageSpec) -> Result<Vec<PathBuf>> {
    let dir = root.join("corpus").join(lang);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        let ext_ok = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| spec.extensions.iter().any(|x| x == e));
        if path.is_file() && ext_ok {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}
