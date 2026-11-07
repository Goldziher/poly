//! The shell dialect shfmt parses a file as, resolved the way the `sh` sidecar does it.
//!
//! The sidecar formats with a filename, so shfmt picks the dialect from the file's name, then its
//! extension, then its shebang (`components/sidecars/sh/format.go`, `dialectFor`). The format cache
//! key is the file's bytes plus this engine's salt, so the same bytes under `a.sh` and `b.bats`
//! would share one cached result unless the path-derived part of the dialect is in the salt. The
//! shebang is part of the bytes, so it needs no salt; only the name and extension do.
//!
//! The classification is kept in step with the Go code by `components/sidecars/sh/testdata/dialects.json`,
//! which the Go test and the test below both read.

use std::path::Path;

/// The dialect a file name or extension fixes, or `None` when the shebang decides.
#[must_use]
pub fn path_dialect(path: &Path) -> Option<&'static str> {
    let base = path.file_name()?.to_str()?;
    match base.strip_prefix('.').unwrap_or(base) {
        "bash_profile" | "bashrc" | "bash_logout" => return Some("bash"),
        "zshenv" | "zprofile" | "zshrc" | "zlogin" | "zlogout" => return Some("zsh"),
        _ => {}
    }
    match &base[base.rfind('.')? + 1..] {
        "bash" => Some("bash"),
        "posix" | "dash" => Some("posix"),
        "mksh" => Some("mksh"),
        "bats" => Some("bats"),
        "zsh" => Some("zsh"),
        _ => None,
    }
}

/// The dialect named by a shell shebang on the first line, as `fileutil.Shebang` reads it:
/// `#!`, optional blanks, `/bin/` or `/usr/bin/`, optional `env` plus blanks, then the shell name
/// followed by whitespace or the end of the input.
fn shebang_dialect(content: &str) -> Option<&'static str> {
    let rest = content.strip_prefix("#!")?.trim_start_matches([' ', '\t']);
    let rest = rest.strip_prefix('/')?;
    let rest = rest.strip_prefix("usr/").unwrap_or(rest);
    let rest = rest.strip_prefix("bin/")?;
    let rest = match rest.strip_prefix("env") {
        Some(after) if after.starts_with([' ', '\t']) => after.trim_start_matches([' ', '\t']),
        _ => rest,
    };
    let end = rest.find(['\t', '\n', '\x0c', '\r', ' ']).unwrap_or(rest.len());
    match &rest[..end] {
        "sh" | "dash" => Some("posix"),
        "bash" => Some("bash"),
        "mksh" => Some("mksh"),
        "bats" => Some("bats"),
        "zsh" => Some("zsh"),
        _ => None,
    }
}

/// The dialect `path` and `content` format as: the name or extension first, then the shebang,
/// then bash.
#[must_use]
pub fn dialect(path: &Path, content: &str) -> &'static str {
    path_dialect(path)
        .or_else(|| shebang_dialect(content))
        .unwrap_or("bash")
}

/// What the formatted output of `path` depends on beyond its bytes: the dialect its name or
/// extension fixes, or `shebang` when the bytes decide.
#[must_use]
pub fn salt(path: &Path) -> String {
    format!("dialect:{}", path_dialect(path).unwrap_or("shebang"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialects_match_the_go_sidecar_table() {
        let table = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../components/sidecars/sh/testdata/dialects.json");
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&table).expect("the shared dialect table")).unwrap();
        assert!(cases.len() > 20, "the shared table is read, not empty");
        for case in &cases {
            let (path, content, want) = (
                case["path"].as_str().unwrap(),
                case["content"].as_str().unwrap(),
                case["dialect"].as_str().unwrap(),
            );
            assert_eq!(dialect(Path::new(path), content), want, "{path} with {content:?}");
        }
    }

    #[test]
    fn only_the_name_and_extension_enter_the_salt() {
        assert_eq!(salt(Path::new("a.sh")), "dialect:shebang");
        assert_eq!(salt(Path::new("dir/.zshrc")), "dialect:zsh");
        assert_ne!(salt(Path::new("a.sh")), salt(Path::new("a.bats")));
    }
}
