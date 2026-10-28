//! Unpacking a downloaded release archive down to the one executable inside it.
//!
//! Nothing here writes a path taken from the archive: the named member is
//! streamed into a file the caller already created, so an entry called
//! `../../.ssh/authorized_keys` has nowhere to go. Entries are matched by their
//! exact normalised path, entries that escape the archive root never match, only
//! regular files are extracted, and the extracted size is capped so a
//! decompression bomb cannot fill the disk.

use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The largest executable poly will extract from an archive.
const MAX_MEMBER_BYTES: u64 = 512 * 1024 * 1024;

/// The packaging of a downloaded archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    TarGz,
    Zip,
}

impl Format {
    /// The format named by the extension of `url`, ignoring a query or fragment.
    pub(super) fn of(url: &str) -> Option<Self> {
        let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
        if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if path.ends_with(".zip") {
            Some(Self::Zip)
        } else {
            None
        }
    }
}

/// Copy the regular file `member` out of the archive `source` into `out`,
/// returning the number of bytes written.
///
/// # Errors
///
/// Returns `Err` when `member` is not a plain relative path, is absent or not a
/// regular file, exceeds the size cap, or the archive is corrupt.
pub(super) fn extract_member<R: Read + Seek, W: Write>(
    source: R,
    format: Format,
    member: &str,
    out: &mut W,
) -> Result<u64> {
    extract_capped(source, format, member, out, MAX_MEMBER_BYTES)
}

fn extract_capped<R: Read + Seek, W: Write>(
    source: R,
    format: Format,
    member: &str,
    out: &mut W,
    cap: u64,
) -> Result<u64> {
    let wanted = member_path(member)?;
    match format {
        Format::TarGz => from_tar_gz(source, &wanted, out, cap),
        Format::Zip => from_zip(source, &wanted, out, cap),
    }
}

fn member_path(member: &str) -> Result<PathBuf> {
    let path = Path::new(member);
    if member.is_empty() || !path.components().all(|part| matches!(part, Component::Normal(_))) {
        bail!("archive member `{member}` must be a relative path without `.` or `..` components");
    }
    Ok(path.to_path_buf())
}

fn normalised(path: &Path) -> PathBuf {
    path.components()
        .filter(|part| !matches!(part, Component::CurDir))
        .collect()
}

fn from_tar_gz<R: Read, W: Write>(source: R, wanted: &Path, out: &mut W, cap: u64) -> Result<u64> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(source));
    for entry in archive.entries().context("reading the tar archive")? {
        let mut entry = entry.context("reading a tar entry")?;
        let is_member = entry.path().is_ok_and(|path| normalised(&path) == wanted);
        if !is_member {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            bail!("`{}` in the archive is not a regular file", wanted.display());
        }
        return copy_capped(&mut entry, out, cap);
    }
    bail!("the archive has no member `{}`", wanted.display())
}

fn from_zip<R: Read + Seek, W: Write>(source: R, wanted: &Path, out: &mut W, cap: u64) -> Result<u64> {
    let mut archive = zip::ZipArchive::new(source).context("reading the zip archive")?;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).context("reading a zip entry")?;
        let is_member = file.enclosed_name().is_some_and(|name| normalised(&name) == wanted);
        if !is_member {
            continue;
        }
        if !file.is_file() {
            bail!("`{}` in the archive is not a regular file", wanted.display());
        }
        return copy_capped(&mut file, out, cap);
    }
    bail!("the archive has no member `{}`", wanted.display())
}

fn copy_capped<R: Read, W: Write>(reader: R, out: &mut W, cap: u64) -> Result<u64> {
    let copied = std::io::copy(&mut reader.take(cap + 1), out).context("extracting the archive member")?;
    if copied > cap {
        bail!("the archive member is larger than the {cap}-byte limit");
    }
    Ok(copied)
}

/// A `.tar.gz` holding `entries`, with names written verbatim (so a test can
/// plant `..` components the tar writer's own API refuses to emit).
#[cfg(test)]
pub(super) fn tar_gz(entries: &[(&str, tar::EntryType, &str)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, kind, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(*kind);
        header.set_cksum();
        builder.append(&header, data.as_bytes()).expect("append");
    }
    let tarball = builder.into_inner().expect("tar");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tarball).expect("gzip");
    encoder.finish().expect("finish gzip")
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tar::EntryType;

    use super::*;

    fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .expect("start");
            writer.write_all(data.as_bytes()).expect("write");
        }
        writer.finish().expect("finish").into_inner()
    }

    fn unpack(archive: Vec<u8>, format: Format, member: &str) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        extract_member(Cursor::new(archive), format, member, &mut out)?;
        Ok(out)
    }

    fn regular<'a>(name: &'a str, data: &'a str) -> (&'a str, EntryType, &'a str) {
        (name, EntryType::Regular, data)
    }

    #[test]
    fn the_format_comes_from_the_url_extension() {
        for (url, expected) in [
            ("https://x/helm-v4.tar.gz", Some(Format::TarGz)),
            ("https://x/tool.TGZ", Some(Format::TarGz)),
            ("https://x/tflint_linux.zip?download=1", Some(Format::Zip)),
            ("https://x/tool", None),
            ("https://x/tool.tar.xz", None),
        ] {
            assert_eq!(Format::of(url), expected, "{url}");
        }
    }

    #[test]
    fn a_tar_gz_member_is_selected_by_its_exact_path() {
        let archive = tar_gz(&[
            regular("linux-amd64/LICENSE", "license"),
            regular("linux-amd64/helm", "the binary"),
            regular("helm", "a different file"),
        ]);
        assert_eq!(
            unpack(archive, Format::TarGz, "linux-amd64/helm").expect("extract"),
            b"the binary"
        );
    }

    #[test]
    fn a_leading_dot_slash_in_the_archive_is_ignored() {
        let archive = tar_gz(&[regular("./bin/tool", "payload")]);
        assert_eq!(unpack(archive, Format::TarGz, "bin/tool").expect("extract"), b"payload");
    }

    #[test]
    fn a_tar_entry_that_escapes_the_root_is_never_the_member() {
        let archive = tar_gz(&[regular("../helm", "evil"), regular("dir/../helm", "evil too")]);
        let error = unpack(archive, Format::TarGz, "helm").expect_err("no member");
        assert!(error.to_string().contains("no member"), "{error:#}");
    }

    #[test]
    fn a_member_path_that_traverses_or_is_absolute_is_refused() {
        for member in ["../evil", "a/../../evil", "/etc/passwd", "./tool", ""] {
            let tarball = tar_gz(&[regular("../evil", "x"), regular("tool", "x")]);
            let error = unpack(tarball, Format::TarGz, member).expect_err(member);
            assert!(error.to_string().contains("relative path"), "{member}: {error:#}");
            let zipped = zip_of(&[("tool", "x")]);
            assert!(unpack(zipped, Format::Zip, member).is_err(), "{member}");
        }
    }

    #[test]
    fn a_missing_member_is_an_error() {
        let archive = tar_gz(&[regular("other", "x")]);
        let error = unpack(archive, Format::TarGz, "helm").expect_err("absent");
        assert!(error.to_string().contains("no member `helm`"), "{error:#}");
    }

    #[test]
    fn a_symlink_or_directory_member_is_refused() {
        for kind in [EntryType::Symlink, EntryType::Directory] {
            let archive = tar_gz(&[("helm", kind, "")]);
            let error = unpack(archive, Format::TarGz, "helm").expect_err("not a file");
            assert!(error.to_string().contains("not a regular file"), "{error:#}");
        }
    }

    #[test]
    fn a_member_over_the_size_cap_is_refused() {
        let payload = "x".repeat(64);
        let archive = tar_gz(&[regular("tool", &payload)]);
        let mut out = Vec::new();
        let error = extract_capped(Cursor::new(archive.clone()), Format::TarGz, "tool", &mut out, 63)
            .expect_err("over the cap");
        assert!(error.to_string().contains("limit"), "{error:#}");
        let mut out = Vec::new();
        let written = extract_capped(Cursor::new(archive), Format::TarGz, "tool", &mut out, 64).expect("at the cap");
        assert_eq!((written, out.len()), (64, 64));
    }

    #[test]
    fn a_zip_member_is_selected_by_its_exact_path() {
        let archive = zip_of(&[("README.md", "docs"), ("tflint", "the binary")]);
        assert_eq!(unpack(archive, Format::Zip, "tflint").expect("extract"), b"the binary");
    }

    #[test]
    fn a_zip_entry_that_escapes_the_root_is_never_the_member() {
        let archive = zip_of(&[("../tflint", "evil")]);
        let error = unpack(archive, Format::Zip, "tflint").expect_err("no member");
        assert!(error.to_string().contains("no member"), "{error:#}");
    }

    #[test]
    fn a_zip_member_over_the_size_cap_is_refused() {
        let big = "x".repeat(32);
        let archive = zip_of(&[("tflint", big.as_str())]);
        let mut out = Vec::new();
        let error = extract_capped(Cursor::new(archive), Format::Zip, "tflint", &mut out, 31).expect_err("over");
        assert!(error.to_string().contains("limit"), "{error:#}");
    }

    #[test]
    fn a_corrupt_archive_is_an_error() {
        assert!(unpack(b"not an archive".to_vec(), Format::TarGz, "x").is_err());
        assert!(unpack(b"not an archive".to_vec(), Format::Zip, "x").is_err());
    }
}
