//! Release pins that live beside a hook config rather than in it.
//!
//! A wrapper script reads `.checkstyle-version` to find the jar it runs. The
//! migration carries that pin into `[tools.<name>] version` so poly runs the
//! same release, but only when poly can actually fetch and verify it.

use std::path::Path;

use poly_catalog::Catalog;

/// The file a repository keeps the pin for `tool` in, when it conventionally does.
pub(super) fn pin_file(tool: &str) -> Option<&'static str> {
    match tool {
        "checkstyle" => Some(".checkstyle-version"),
        "palantir-java-format" => Some(".palantir-java-format-version"),
        _ => None,
    }
}

/// The version on the first line of a pin file: its first word, so
/// `12.3.1 <sha256>` and a bare `2.102.0` both read.
pub(super) fn read_version(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let version = text.lines().next()?.split_whitespace().next()?.trim_start_matches('v');
    (!version.is_empty()).then(|| version.to_string())
}

/// Whether the catalog carries a pinned, checksummed release of `tool`.
pub(super) fn catalog_has_release(tool: &str, version: &str) -> bool {
    Catalog::get()
        .tool(tool)
        .and_then(|tool| tool.recipe.as_ref())
        .and_then(|recipe| recipe.install.as_ref())
        .is_some_and(|install| install.releases.contains_key(version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_file_with_a_checksum_yields_just_the_version() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join(".checkstyle-version");
        std::fs::write(&file, "12.3.1 4ecdfa85\n").expect("write");
        assert_eq!(read_version(&file).as_deref(), Some("12.3.1"));
        std::fs::write(&file, "v2.102.0\n").expect("write");
        assert_eq!(read_version(&file).as_deref(), Some("2.102.0"));
        assert_eq!(read_version(&dir.path().join("absent")), None);
    }

    #[test]
    fn the_catalog_knows_the_releases_armis_pins() {
        assert!(catalog_has_release("checkstyle", "12.3.1"));
        assert!(catalog_has_release("palantir-java-format", "2.102.0"));
        assert!(!catalog_has_release("checkstyle", "0.0.1"));
    }
}
