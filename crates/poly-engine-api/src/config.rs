//! The per-engine configuration slice handed to every [`Engine`](crate::engine::Engine) call.

pub use poly_config::{GlobalDefaults, LineEnding};

/// The slice of config handed to one engine for one file.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Global opinionated defaults.
    pub globals: GlobalDefaults,
    /// Indent width for this file's language.
    pub indent_width: usize,
    /// Tool-specific options from `[<kind>.<lang>.<engine>]` (engine merges its own defaults).
    pub options: toml::Table,
}

impl EngineConfig {
    /// A stable 32-byte blake3 digest of everything in this config: the global
    /// defaults, `indent_width` and the `options` table.
    ///
    /// Component hosts use it to key per-config state (a configured engine
    /// instance, a cached option-key answer) without re-sending or re-serializing
    /// the config. The options table serializes in key order (`toml::Table` is
    /// ordered), so equal configs hash equal across processes.
    ///
    /// Computed on demand rather than memoised in a field: `EngineConfig` is built
    /// with struct literals in hundreds of places, and a private cache field would
    /// break them all; a cache also goes stale when a clone's `options` are mutated.
    /// Callers that need it per file compute it once per engine plan and hold the
    /// result.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(format!("{:?}", self.globals).as_bytes());
        hasher.update(b"\0");
        hasher.update(&(self.indent_width as u64).to_le_bytes());
        hasher.update(b"\0");
        match toml::to_string(&self.options) {
            Ok(text) => hasher.update(text.as_bytes()),
            // Unserializable tables still hash deterministically through Debug.
            Err(_) => hasher.update(format!("{:?}", self.options).as_bytes()),
        };
        *hasher.finalize().as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(options: &str) -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options: toml::from_str(options).expect("valid toml"),
        }
    }

    #[test]
    fn fingerprint_is_stable_and_clone_independent() {
        let a = cfg("a = 1\nb = \"x\"");
        let mut b = a.clone();
        assert_eq!(a.fingerprint(), b.fingerprint());
        b.options.insert("c".into(), toml::Value::Boolean(true));
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprint_ignores_key_order_and_sees_indent() {
        let a = cfg("a = 1\nb = 2");
        let b = cfg("b = 2\na = 1");
        assert_eq!(a.fingerprint(), b.fingerprint());
        let mut c = a.clone();
        c.indent_width = 2;
        assert_ne!(a.fingerprint(), c.fingerprint());
    }
}
