//! Turning wire file descriptors plus the call `body` into [`SourceFile`]s.

use std::path::PathBuf;
use std::sync::Arc;

use poly_component_protocol::{WireError, WireFile, WireFileSlice};
use poly_engine_api::engine::SourceFile;

use super::errors::protocol;
use super::language::language_from_id;

fn source(path: &str, language: &str, bytes: &[u8]) -> Result<SourceFile, WireError> {
    let text = std::str::from_utf8(bytes).map_err(|e| protocol(format!("`{path}` is not valid UTF-8: {e}")))?;
    Ok(SourceFile {
        path: PathBuf::from(path),
        language: language_from_id(language),
        content: Arc::from(text),
    })
}

/// The single file of a call: its content is the whole `body`.
pub(crate) fn single(file: &WireFile, body: &[u8]) -> Result<SourceFile, WireError> {
    if u64::try_from(body.len()).ok() != Some(file.len) {
        return Err(protocol(format!(
            "`{}` declares {} byte(s) but the body holds {}",
            file.path,
            file.len,
            body.len()
        )));
    }
    source(&file.path, &file.language, body)
}

/// The members of a batch: each a checked range of `body`.
pub(crate) fn slices(files: &[WireFileSlice], body: &[u8]) -> Result<Vec<SourceFile>, WireError> {
    files
        .iter()
        .map(|file| {
            let range = usize::try_from(file.offset)
                .ok()
                .zip(usize::try_from(file.len).ok())
                .and_then(|(start, len)| Some(start..start.checked_add(len)?))
                .and_then(|range| body.get(range))
                .ok_or_else(|| {
                    protocol(format!(
                        "`{}` slice (offset {}, len {}) lies outside the {}-byte body",
                        file.path,
                        file.offset,
                        file.len,
                        body.len()
                    ))
                })?;
            source(&file.path, &file.language, range)
        })
        .collect()
}
