//! The one poly-side option a native tool takes beyond `enabled`.
//!
//! It lives in its own module so that the option-key audit can attribute it.
//! That audit derives "which keys does this backend read" by scanning the
//! backend's declared sources, and every wrapped toolchain binary shares
//! `native_tool/mod.rs` — so a read sited there is attributed to all eleven of
//! them, and satisfying the audit would mean declaring `use_tabs` on tools that
//! ignore it. Declaring a key a backend does not honour is the exact defect the
//! audit exists to catch, so the read is sited where only shfmt claims it. ~keep

use crate::config::EngineConfig;

/// Whether this tool should indent with tabs rather than spaces.
///
/// Only meaningful for specs carrying `format_indent_flag`; shfmt is the one
/// such tool today. It cannot be expressed through the shared `indent_width`,
/// which `Config::engine_config` filters `0` out of as "unset" for every engine
/// — and `0` is precisely what shfmt reads as "indent with tabs". Off unless
/// asked for: tab-indented shell is common, and a formatter that reindents the
/// whole tree on adoption is one that does not get adopted.
pub(super) fn use_tabs(cfg: &EngineConfig) -> bool {
    cfg.options
        .get("use_tabs")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
}
