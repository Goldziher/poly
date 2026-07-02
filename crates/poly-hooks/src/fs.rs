//! Path utilities — clean, simplify, normalize, and make-relative.
//!
//! Ported from `polyhooks/src/fs.rs` (MIT, © 2023 Astral Software Inc.).
//! Tokio-dependent helpers (`LockedFile`, `symlink_or_copy`) are intentionally
//! omitted; they belong to a later phase or a separate crate.

use std::fmt::Display;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

/// The process's working directory at startup, canonicalized.
pub static CWD: LazyLock<PathBuf> = LazyLock::new(|| {
    std::env::current_dir()
        .map(|cwd| dunce::canonicalize(&cwd).unwrap_or(cwd))
        .expect("current directory must exist")
});

/// Expand a path starting with `~` to the user's home directory.
pub fn expand_tilde(path: PathBuf) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix("~") {
        if let Some(home) = std::env::home_dir() {
            return home.join(stripped);
        }
    }
    path
}

/// Normalise a path to use `/` as separator everywhere (no-op outside Windows).
#[cfg(not(windows))]
pub fn normalize_path(path: PathBuf) -> PathBuf {
    path
}

/// Normalise a path to use `/` as separator everywhere (replaces `\` on Windows).
#[cfg(windows)]
pub fn normalize_path(path: PathBuf) -> PathBuf {
    use std::ffi::OsString;

    if !path.as_os_str().as_encoded_bytes().contains(&b'\\') {
        return path;
    }

    let mut path = path.into_os_string().into_encoded_bytes();
    for byte in &mut path {
        if *byte == b'\\' {
            *byte = b'/';
        }
    }

    // SAFETY: `path` came from `OsString::into_encoded_bytes` and we only
    // replace ASCII `\` with ASCII `/`. ASCII bytes cannot appear inside a
    // non-ASCII UTF-8/WTF-8 sequence, so the encoding invariant is preserved.
    PathBuf::from(unsafe { OsString::from_encoded_bytes_unchecked(path) })
}

/// Compute a relative path from `base` to `path`.
///
/// Returns `Err` when no relative path exists (e.g. different drive letters
/// on Windows).
pub fn relative_to(path: impl AsRef<Path>, base: impl AsRef<Path>) -> Result<PathBuf, std::io::Error> {
    let (stripped, common_prefix) = base
        .as_ref()
        .ancestors()
        .find_map(|ancestor| {
            dunce::simplified(path.as_ref())
                .strip_prefix(dunce::simplified(ancestor))
                .ok()
                .map(|stripped| (stripped, ancestor))
        })
        .ok_or_else(|| {
            std::io::Error::other(format!(
                "no relative path: {} vs. {}",
                path.as_ref().display(),
                base.as_ref().display()
            ))
        })?;

    let levels_up = base.as_ref().components().count() - common_prefix.components().count();
    let up = std::iter::repeat_n("..", levels_up).collect::<PathBuf>();
    Ok(up.join(stripped))
}

/// Display / simplification helpers for [`Path`]-like types.
pub trait Simplified {
    /// Strip any `\\?\` prefix on Windows; identity elsewhere.
    fn simplified(&self) -> &Path;

    /// Return a simplified display of the path.
    fn simplified_display(&self) -> impl Display;

    /// Return a display of the path relative to the current working directory.
    fn user_display(&self) -> impl Display;
}

impl<T: AsRef<Path>> Simplified for T {
    fn simplified(&self) -> &Path {
        dunce::simplified(self.as_ref())
    }

    fn simplified_display(&self) -> impl Display {
        dunce::simplified(self.as_ref()).display()
    }

    fn user_display(&self) -> impl Display {
        let path = dunce::simplified(self.as_ref());

        // If CWD is the filesystem root, display as-is.
        if CWD.ancestors().nth(1).is_none() {
            return path.display();
        }

        let path = path.strip_prefix(CWD.simplified()).unwrap_or(path);
        path.display()
    }
}

/// Lexical path cleaning (no filesystem access).
pub trait PathClean {
    /// Return a clean version of this path with `.` and `..` resolved.
    fn clean(&self) -> PathBuf;
}

impl PathClean for Path {
    fn clean(&self) -> PathBuf {
        clean_path(self)
    }
}

impl PathClean for PathBuf {
    fn clean(&self) -> PathBuf {
        clean_path(self.as_path())
    }
}

fn clean_path(path: &Path) -> PathBuf {
    let mut out: Vec<Component<'_>> = Vec::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::RootDir) => {}
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                None | Some(Component::CurDir | Component::ParentDir | Component::Prefix(_)) => {
                    out.push(component);
                }
            },
            c => out.push(c),
        }
    }

    if out.is_empty() {
        PathBuf::from(".")
    } else {
        out.iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    #[test]
    fn path_clean_various() {
        use super::PathClean as _;

        let cases = [
            ("", "."),
            (".", "."),
            ("./test/./path", "test/path"),
            ("test/path/..", "test"),
            ("test/path/../../..", ".."),
            ("test/path/../../../..", "../.."),
            ("../test/..", ".."),
            ("/../test", "/test"),
            ("/test/path/../../../..", "/"),
        ];

        for (input, expected) in cases {
            assert_eq!(Path::new(input).clean(), PathBuf::from(expected), "{input}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn normalize_path_noop_on_non_windows() {
        let path = PathBuf::from(r"foo\bar/baz");
        assert_eq!(super::normalize_path(path.clone()), path);
    }
}
