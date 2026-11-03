//! Process-wide memo over `tree_sitter_language_pack::get_language`.
//!
//! The pack's lookup is lock-free only on a hit. A grammar that is not present
//! locally (and cannot be fetched) takes a global download lock and re-attempts
//! the fetch on every call, so with one lookup per file every rayon worker ends
//! up queueing behind that lock. Resolving each grammar once per process,
//! success or failure, removes the contention and keeps the time a file spends
//! in an engine down to the work done on that file.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tree_sitter_language_pack::Language;

type Slot = Arc<OnceLock<Option<Language>>>;

/// The grammar named `name`, resolved at most once per process. `None` means
/// the pack cannot provide it; that answer is remembered too.
///
/// Offline (`poly_config::network::offline`), only grammars the pack can load
/// without a download count: an uncached one is absent, exactly like an unknown
/// name (callers then degrade as they do for any missing grammar).
pub(crate) fn language(name: &str) -> Option<Language> {
    static SLOTS: OnceLock<Mutex<HashMap<String, Slot>>> = OnceLock::new();
    let slot = {
        let mut slots = SLOTS
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(slots.entry(name.to_owned()).or_default())
    };
    slot.get_or_init(|| resolve(name)).clone()
}

fn resolve(name: &str) -> Option<Language> {
    // `has_parser` answers from the static table and the download cache and never
    // fetches; `get_language` would take the download lock and hit the network.
    if poly_config::network::offline() && !tree_sitter_language_pack::has_parser(name) {
        return None;
    }
    tree_sitter_language_pack::get_language(name).ok()
}

#[cfg(test)]
mod tests {
    use super::language;

    #[test]
    fn resolves_a_known_grammar_repeatedly() {
        assert!(language("python").is_some());
        assert!(language("python").is_some());
    }

    #[test]
    fn remembers_an_unknown_grammar_as_absent() {
        assert!(language("definitely-not-a-grammar").is_none());
        assert!(language("definitely-not-a-grammar").is_none());
    }

    #[test]
    fn concurrent_lookups_agree() {
        let found: Vec<bool> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8).map(|_| scope.spawn(|| language("rust").is_some())).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(found.iter().all(|&ok| ok));
    }
}
