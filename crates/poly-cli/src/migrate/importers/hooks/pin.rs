//! Release pins that live beside a hook config rather than in it.
//!
//! A wrapper script reads `.checkstyle-version` to find the jar it runs; a repo
//! pins `golangci-lint` in `.tool-versions`; a pre-commit hook's `rev` is the tool
//! release. The migration carries such a pin into `[tools.<name>] version` so poly
//! runs the same release, but only when poly can actually fetch and verify it.

use std::path::Path;

use poly_catalog::Catalog;

/// A release a repository pins for a tool, and where it says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Pin {
    /// The file holding the pin, relative to the config directory.
    pub file: String,
    /// The pinned version, without a leading `v`.
    pub version: String,
}

/// The files a repository conventionally keeps the pin for `tool` in, best first.
fn pin_files(tool: &str) -> &'static [&'static str] {
    match tool {
        "checkstyle" => &[".checkstyle-version"],
        "palantir-java-format" => &[".palantir-java-format-version"],
        "buildifier" => &[".buildifier-version"],
        "buf" => &[".buf-version"],
        "golangci-lint" => &[".golangci-lint-version", ".golangci-version"],
        "helm" | "helm-template" => &[".helm-version"],
        "tflint" => &[".tflint-version"],
        "hadolint" => &[".hadolint-version"],
        "actionlint" => &[".actionlint-version"],
        "terraform" => &[".terraform-version"],
        "tofu" => &[".opentofu-version"],
        _ => &[],
    }
}

/// What asdf and mise call `tool` in `.tool-versions`.
fn tool_versions_name(tool: &str) -> &str {
    match tool {
        "helm-template" => "helm",
        "tofu" => "opentofu",
        other => other,
    }
}

/// The release `dir` pins for `tool`: the first of its conventional pin files that
/// names one, else its `.tool-versions` line.
pub(super) fn pin_for(dir: &Path, tool: &str) -> Option<Pin> {
    for file in pin_files(tool) {
        if let Some(version) = read_version(&dir.join(file)) {
            return Some(Pin {
                file: (*file).to_owned(),
                version,
            });
        }
    }
    let wanted = tool_versions_name(tool);
    let text = std::fs::read_to_string(dir.join(".tool-versions")).ok()?;
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            if words.next()? != wanted {
                return None;
            }
            words.next()
        })
        .next()
        .map(|version| Pin {
            file: ".tool-versions".to_owned(),
            version: version.trim_start_matches('v').to_owned(),
        })
}

/// The version on the first line of a pin file: its first word, so
/// `12.3.1 <sha256>` and a bare `2.102.0` both read.
pub(super) fn read_version(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let version = text.lines().next()?.split_whitespace().next()?.trim_start_matches('v');
    (!version.is_empty()).then(|| version.to_string())
}

/// Whether the catalog carries any pinned, checksummed release of `tool`, i.e.
/// whether `[tools.<name>] version` can make poly download it.
pub(super) fn has_releases(tool: &str) -> bool {
    Catalog::get()
        .tool(tool)
        .and_then(|tool| tool.recipe.as_ref())
        .and_then(|recipe| recipe.install.as_ref())
        .is_some_and(|install| !install.releases.is_empty())
}

/// Whether the catalog carries a pinned, checksummed release of `tool` at `version`.
pub(super) fn catalog_has_release(tool: &str, version: &str) -> bool {
    Catalog::get()
        .tool(tool)
        .and_then(|tool| tool.recipe.as_ref())
        .and_then(|recipe| recipe.install.as_ref())
        .is_some_and(|install| install.releases.contains_key(version))
}

/// What to tell the user about a pin poly cannot carry out: the tool is
/// downloadable but not at this release, or poly runs whatever the `PATH` holds.
pub(super) fn unpinnable(tool: &str, source: &str, version: &str) -> Unpinnable {
    if has_releases(tool) {
        Unpinnable::Release(format!(
            "{source} pins {tool} {version}, which poly cannot download; set `[tools.{tool}] version`, `url` and \
             `sha256` to pin it"
        ))
    } else {
        Unpinnable::OnPath(format!(
            "{source} pins {tool} {version}; poly has no download for {tool} and runs the one on PATH (or \
             `[tools.{tool}] bin`), so install that release"
        ))
    }
}

/// A pin poly could not carry out, split by how much it matters.
pub(super) enum Unpinnable {
    /// A downloadable tool at a release the catalog lacks: the run would differ.
    Release(String),
    /// A tool poly never downloads: informational.
    OnPath(String),
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
        assert!(catalog_has_release("buildifier", "8.5.1"));
        assert!(!catalog_has_release("checkstyle", "0.0.1"));
    }

    #[test]
    fn dedicated_pin_files_exist_for_the_tools_that_conventionally_have_them() {
        let dir = tempfile::tempdir().expect("dir");
        for (file, tool) in [
            (".golangci-lint-version", "golangci-lint"),
            (".buf-version", "buf"),
            (".helm-version", "helm-template"),
            (".tflint-version", "tflint"),
            (".hadolint-version", "hadolint"),
            (".buildifier-version", "buildifier"),
        ] {
            std::fs::write(dir.path().join(file), "v9.9.9\n").expect("write");
            assert_eq!(
                pin_for(dir.path(), tool),
                Some(Pin {
                    file: file.to_string(),
                    version: "9.9.9".to_string()
                }),
                "{tool}"
            );
        }
    }

    #[test]
    fn tool_versions_is_read_for_the_asdf_name_of_the_tool() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(
            dir.path().join(".tool-versions"),
            "# pins\nnodejs 22.1.0\ngolangci-lint v2.1.6\nopentofu 1.9.0\nhelm 3.16.0\n",
        )
        .expect("write");
        let version = |tool: &str| pin_for(dir.path(), tool).map(|pin| (pin.file, pin.version));
        assert_eq!(
            version("golangci-lint"),
            Some((".tool-versions".to_string(), "2.1.6".to_string()))
        );
        assert_eq!(version("tofu").map(|pin| pin.1).as_deref(), Some("1.9.0"));
        assert_eq!(version("helm-template").map(|pin| pin.1).as_deref(), Some("3.16.0"));
        assert_eq!(version("buf"), None);
    }

    #[test]
    fn a_dedicated_pin_file_wins_over_tool_versions() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join(".tool-versions"), "buf 1.0.0\n").expect("write");
        std::fs::write(dir.path().join(".buf-version"), "1.2.3\n").expect("write");
        assert_eq!(pin_for(dir.path(), "buf").map(|pin| pin.version).as_deref(), Some("1.2.3"));
    }

    #[test]
    fn an_unpinnable_release_is_serious_only_for_a_downloadable_tool() {
        assert!(matches!(
            unpinnable("checkstyle", ".checkstyle-version", "1.0.0"),
            Unpinnable::Release(_)
        ));
        assert!(matches!(
            unpinnable("terraform", ".tool-versions", "1.0.0"),
            Unpinnable::OnPath(_)
        ));
    }
}
