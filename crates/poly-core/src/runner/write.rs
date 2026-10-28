//! Atomic file writes for formatted and autofixed output.
//!
//! Split out of `runner.rs` to keep that file under the module-size cap; the
//! runner calls [`write_atomic`] wherever `--fix` or `fmt --fix` lands content.

/// Replace `path` with `contents` via a sibling temp file and a rename, so a
/// crash or a concurrent reader never sees a half-written file, preserving the
/// original file's permissions (an executable script keeps its exec bit).
pub(super) fn write_atomic(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    use std::io::Write as _;

    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("poly");
    let tmp = parent.join(format!(".{file_name}.{}.poly.tmp", std::process::id()));
    let original_permissions = std::fs::metadata(path).ok().map(|m| m.permissions());
    let written = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        // ~keep The rename replaces the original inode with a freshly created temp file, whose mode is
        // `0666 & !umask` and has no relationship to the file being formatted. Without this, formatting
        // an executable script silently clears its exec bit. Creating the file with the original mode
        // makes the follow-up `fchmod` a no-op in the common case, so it is skipped then.
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
            if let Some(permissions) = &original_permissions {
                options.mode(permissions.mode() & 0o7777);
            }
        }
        let mut file = options.open(&tmp)?;
        file.write_all(contents.as_bytes())?;
        if let Some(permissions) = original_permissions
            && file.metadata()?.permissions() != permissions
        {
            file.set_permissions(permissions)?;
        }
        Ok(())
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(error.into());
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error.into());
    }
    Ok(())
}
