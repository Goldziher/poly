//! The slice of the GitHub releases API that resolution reads.

use serde::Deserialize;

/// A release as the API reports it.
#[derive(Debug, Deserialize)]
pub(super) struct ApiRelease {
    pub(super) tag_name: String,
    #[serde(default)]
    pub(super) assets: Vec<ApiAsset>,
}

/// A release asset; `digest` is `sha256:<hex>` on every asset uploaded since mid-2025.
#[derive(Debug, Deserialize)]
pub(super) struct ApiAsset {
    pub(super) name: String,
    pub(super) browser_download_url: String,
    #[serde(default)]
    pub(super) digest: Option<String>,
}

/// Parse an API body.
///
/// # Errors
///
/// Returns a message naming `url` when the body is not the expected JSON.
pub(super) fn parse(body: &str, url: &str) -> Result<ApiRelease, String> {
    serde_json::from_str(body).map_err(|error| format!("{url} did not return a release: {error}"))
}
