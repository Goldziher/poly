//! Provisioning: downloading a pinned, checksummed release of a tool into poly's
//! cache.
//!
//! Only ever reached when the config pins a `version` *and* sets
//! `install = "auto"`; poly does not fetch executables on its own initiative.
//! The download is verified against a SHA-256 digest before it is made
//! executable and renamed into place, so a truncated or substituted file is never
//! left where a later run would trust it.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use poly_catalog::{Asset, Release, ReleaseKind};
use sha2::{Digest, Sha256};

/// Platform key of the running host in the catalog's asset tables.
pub(super) fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

/// The asset of `release` for this host, falling back to the platform-neutral
/// `any` entry.
pub(super) fn asset_for(release: &Release) -> Option<&Asset> {
    release.assets.get(&platform()).or_else(|| release.assets.get("any"))
}

/// Where a release of `tool` is stored: `<cache>/tools/<tool>/<version>/<file>`.
pub(super) fn destination(tool: &str, version: &str, asset: &Asset) -> Result<PathBuf> {
    let file = asset.url.rsplit('/').next().filter(|f| !f.is_empty()).unwrap_or(tool);
    Ok(poly_cache::cache_home()?
        .join("tools")
        .join(tool)
        .join(version)
        .join(file))
}

/// Whether `path` is a complete, verified install of `asset`. The digest is
/// recorded next to the file when it is verified, so later runs compare a short
/// stamp instead of hashing 90 MB.
pub(super) fn is_installed(path: &Path, asset: &Asset) -> bool {
    path.is_file() && std::fs::read_to_string(stamp(path)).is_ok_and(|stamped| stamped.trim() == asset.sha256)
}

fn stamp(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".sha256");
    PathBuf::from(name)
}

/// Download `asset` to `path` unless it is already installed, verifying the
/// digest. Returns the installed path.
///
/// # Errors
///
/// Returns `Err` on a network failure, a digest mismatch, or an unwritable
/// cache directory.
pub(super) fn ensure(path: &Path, asset: &Asset, kind: ReleaseKind) -> Result<()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let _held = LOCK.lock().expect("install lock poisoned");
    if is_installed(path, asset) {
        return Ok(());
    }
    let directory = path.parent().context("install path has no parent directory")?;
    std::fs::create_dir_all(directory).with_context(|| format!("creating {}", directory.display()))?;
    let partial = tempfile::Builder::new()
        .prefix(".download-")
        .tempfile_in(directory)
        .with_context(|| format!("creating a download file in {}", directory.display()))?;
    let digest = download(&asset.url, partial.as_file())?;
    if digest != asset.sha256 {
        bail!(
            "downloaded {} has sha256 {digest}, expected {}; refusing to install it",
            asset.url,
            asset.sha256
        );
    }
    #[cfg(unix)]
    if kind == ReleaseKind::Binary {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(partial.path(), std::fs::Permissions::from_mode(0o755))
            .context("marking the download executable")?;
    }
    let _ = kind;
    partial
        .persist(path)
        .with_context(|| format!("installing to {}", path.display()))?;
    std::fs::write(stamp(path), format!("{}\n", asset.sha256)).context("recording the install digest")?;
    Ok(())
}

/// Stream `url` into `target`, returning the hex SHA-256 of what was written.
fn download(url: &str, target: &File) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .into();
    let mut response = agent.get(url).call().with_context(|| format!("downloading {url}"))?;
    let mut reader = response.body_mut().as_reader();
    let mut writer = std::io::BufWriter::new(target);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).with_context(|| format!("reading {url}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        writer.write_all(&buffer[..read]).context("writing the download")?;
    }
    writer.flush().context("flushing the download")?;
    Ok(hex(&hasher.finalize()))
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    fn serve(body: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let mut stream = stream.expect("accept");
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).expect("head");
                stream.write_all(body).expect("body");
            }
        });
        format!("http://{address}/tool-1.0")
    }

    fn sha(body: &[u8]) -> String {
        hex(&Sha256::digest(body))
    }

    #[test]
    fn a_verified_download_is_installed_executable_and_stamped() {
        let url = serve(b"#!/bin/sh\nexit 0\n");
        let asset = Asset {
            url,
            sha256: sha(b"#!/bin/sh\nexit 0\n"),
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tool-1.0");
        ensure(&path, &asset, ReleaseKind::Binary).expect("install");
        assert!(is_installed(&path, &asset));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).expect("meta").permissions().mode() & 0o111,
                0o111
            );
        }
    }

    #[test]
    fn a_digest_mismatch_installs_nothing() {
        let url = serve(b"not what was pinned");
        let asset = Asset {
            url,
            sha256: sha(b"what was pinned"),
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tool-1.0");
        let error = ensure(&path, &asset, ReleaseKind::Binary).expect_err("must refuse");
        assert!(error.to_string().contains("refusing to install"), "{error:#}");
        assert!(!path.exists(), "a mismatched download must not be left behind");
        assert!(!is_installed(&path, &asset));
    }

    #[test]
    fn an_installed_tool_is_not_downloaded_again() {
        let body: &'static [u8] = b"payload";
        let asset = Asset {
            url: "http://127.0.0.1:1/unreachable".to_owned(),
            sha256: sha(body),
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tool");
        std::fs::write(&path, body).expect("write");
        std::fs::write(stamp(&path), format!("{}\n", asset.sha256)).expect("stamp");
        ensure(&path, &asset, ReleaseKind::Binary).expect("already installed, so no network is needed");
    }

    #[test]
    fn platform_key_matches_the_catalog_spelling() {
        let key = platform();
        assert!(key.contains('-'), "{key}");
        assert!(!key.starts_with("macos"), "the catalog says darwin: {key}");
    }
}
