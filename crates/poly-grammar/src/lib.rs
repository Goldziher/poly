//! Process-wide memo over `tree_sitter_language_pack::get_language`.
//!
//! The pack's lookup is lock-free only on a hit. A grammar that is not present
//! locally takes a global download lock and re-attempts the fetch on every
//! call, so with one lookup per file every rayon worker ends up queueing behind
//! that lock. Each grammar is therefore resolved once and the answer kept:
//!
//! - a grammar that loads, or a name the pack does not know, is remembered for
//!   the life of the process;
//! - a grammar that could not be fetched (network or I/O failure) is remembered
//!   only for [`RETRY_AFTER`]. Lookups inside that window return the recorded
//!   failure without touching the download lock, so a failing fetch costs one
//!   attempt per window rather than one per file, yet a transient outage heals
//!   instead of poisoning the rest of the run.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use tree_sitter_language_pack::Language;

/// How long a failed download is remembered before the next attempt.
pub const RETRY_AFTER: Duration = Duration::from_secs(30);

/// Why a grammar could not be provided.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrammarError {
    /// The pack has no grammar of this name. Permanent for the process.
    #[error("unknown grammar '{0}'")]
    Unknown(String),
    /// The grammar exists but is not downloaded and poly is offline. Permanent
    /// for the process: offline mode does not change mid-run.
    #[error(
        "grammar '{0}' is not downloaded and poly is offline (--offline / POLY_OFFLINE); \
         run once online to fetch it"
    )]
    NotCached(String),
    /// The grammar exists but could not be loaded or downloaded. Retried after
    /// [`RETRY_AFTER`].
    #[error("grammar '{name}' is unavailable: {reason}")]
    Unavailable {
        /// Grammar name.
        name: String,
        /// The underlying download or I/O failure.
        reason: String,
    },
}

#[derive(Default)]
enum State {
    #[default]
    Empty,
    /// A permanent failure ([`GrammarError::Unknown`] or [`GrammarError::NotCached`]).
    Settled(GrammarError),
    Failed {
        at: Instant,
        reason: String,
    },
}

/// One grammar name's memo. `found` is read without locking; `state` only
/// serializes attempts and holds the failure bookkeeping.
struct Slot<T> {
    found: OnceLock<T>,
    state: Mutex<State>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            found: OnceLock::new(),
            state: Mutex::new(State::Empty),
        }
    }
}

/// Per-name memo with the success/permanent-failure/transient-failure semantics
/// above. Generic over the value so the semantics are testable without a grammar.
struct Memo<T> {
    slots: RwLock<HashMap<String, Arc<Slot<T>>>>,
}

impl<T: Clone> Memo<T> {
    fn new() -> Self {
        Self {
            slots: RwLock::new(HashMap::new()),
        }
    }

    fn slot(&self, name: &str) -> Arc<Slot<T>> {
        if let Some(slot) = self
            .slots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
        {
            return Arc::clone(slot);
        }
        let mut slots = self.slots.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(slots.entry(name.to_owned()).or_default())
    }

    fn get(
        &self,
        name: &str,
        now: Instant,
        resolve: impl FnOnce() -> Result<T, GrammarError>,
    ) -> Result<T, GrammarError> {
        let slot = self.slot(name);
        // Hit: no mutex, so workers sharing a grammar never serialize.
        if let Some(value) = slot.found.get() {
            return Ok(value.clone());
        }
        // Held across `resolve` so concurrent callers for one name wait for a
        // single attempt rather than each starting their own.
        let mut state = slot.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(value) = slot.found.get() {
            return Ok(value.clone());
        }
        match &*state {
            State::Settled(error) => return Err(error.clone()),
            State::Failed { at, reason } if now.saturating_duration_since(*at) < RETRY_AFTER => {
                return Err(GrammarError::Unavailable {
                    name: name.to_owned(),
                    reason: reason.clone(),
                });
            }
            State::Failed { .. } | State::Empty => {}
        }
        match resolve() {
            Ok(value) => {
                let _ = slot.found.set(value.clone());
                Ok(value)
            }
            Err(error @ (GrammarError::Unknown(_) | GrammarError::NotCached(_))) => {
                *state = State::Settled(error.clone());
                Err(error)
            }
            Err(error) => {
                if let GrammarError::Unavailable { reason, .. } = &error {
                    *state = State::Failed {
                        at: now,
                        reason: reason.clone(),
                    };
                }
                Err(error)
            }
        }
    }
}

/// The grammar named `name`, or why it is not available.
///
/// Offline (`poly_config::network::offline`), only grammars the pack can load
/// without a download count: an uncached one is [`GrammarError::NotCached`].
pub fn try_language(name: &str) -> Result<Language, GrammarError> {
    static MEMO: OnceLock<Memo<Language>> = OnceLock::new();
    MEMO.get_or_init(Memo::new).get(name, Instant::now(), || resolve(name))
}

/// [`try_language`] with the reason dropped. Callers that must tell a missing
/// grammar from a failed download use [`try_language`].
pub fn language(name: &str) -> Option<Language> {
    try_language(name).ok()
}

fn resolve(name: &str) -> Result<Language, GrammarError> {
    // `has_parser` answers from the static table and the download cache and never
    // fetches; `get_language` would take the download lock and hit the network.
    if let Some(error) = offline_gate(name, poly_config::network::offline(), || {
        tree_sitter_language_pack::has_parser(name)
    }) {
        return Err(error);
    }
    tree_sitter_language_pack::get_language(name).map_err(|error| classify(name, &error))
}

/// The error for a lookup that must not download: offline with the grammar not
/// locally loadable. `loadable` is only consulted when offline.
fn offline_gate(name: &str, offline: bool, loadable: impl FnOnce() -> bool) -> Option<GrammarError> {
    (offline && !loadable()).then(|| GrammarError::NotCached(name.to_owned()))
}

fn classify(name: &str, error: &tree_sitter_language_pack::Error) -> GrammarError {
    match error {
        tree_sitter_language_pack::Error::LanguageNotFound(_) => GrammarError::Unknown(name.to_owned()),
        other => GrammarError::Unavailable {
            name: name.to_owned(),
            reason: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn unavailable(name: &str) -> GrammarError {
        GrammarError::Unavailable {
            name: name.into(),
            reason: "503".into(),
        }
    }

    #[test]
    fn transient_failure_is_not_cached_past_the_retry_window() {
        let memo = Memo::<u32>::new();
        let t0 = Instant::now();
        let calls = Cell::new(0);
        let fail = || {
            calls.set(calls.get() + 1);
            Err(unavailable("go"))
        };
        assert!(memo.get("go", t0, fail).is_err());
        assert!(memo.get("go", t0 + Duration::from_secs(1), fail).is_err());
        assert_eq!(
            calls.get(),
            1,
            "inside the window the failure is served without a new attempt"
        );
        let later = t0 + RETRY_AFTER + Duration::from_secs(1);
        assert_eq!(memo.get("go", later, || Ok(7)), Ok(7));
        assert_eq!(memo.get("go", later, || panic!("success is cached")), Ok(7));
    }

    #[test]
    fn failure_message_survives_inside_the_window() {
        let memo = Memo::<u32>::new();
        let t0 = Instant::now();
        let _ = memo.get("go", t0, || Err(unavailable("go")));
        let err = memo.get("go", t0, || panic!("no retry")).unwrap_err();
        assert_eq!(err.to_string(), "grammar 'go' is unavailable: 503");
    }

    #[test]
    fn unknown_name_is_cached_forever() {
        let memo = Memo::<u32>::new();
        let t0 = Instant::now();
        let err = memo.get("nope", t0, || Err(GrammarError::Unknown("nope".into())));
        assert_eq!(err, Err(GrammarError::Unknown("nope".into())));
        let far = t0 + RETRY_AFTER * 100;
        assert_eq!(
            memo.get("nope", far, || panic!("no retry")),
            Err(GrammarError::Unknown("nope".into()))
        );
    }

    #[test]
    fn classify_separates_missing_from_download_failure() {
        use tree_sitter_language_pack::Error;
        assert_eq!(
            classify("x", &Error::LanguageNotFound("x".into())),
            GrammarError::Unknown("x".into())
        );
        let got = classify("x", &Error::Download("HTTP 503".into()));
        assert!(matches!(&got, GrammarError::Unavailable { reason, .. } if reason.contains("HTTP 503")));
    }

    #[test]
    fn offline_uncached_is_not_cached_not_unknown() {
        assert_eq!(
            offline_gate("go", true, || false),
            Some(GrammarError::NotCached("go".into()))
        );
        assert_eq!(offline_gate("go", true, || true), None);
        assert_eq!(offline_gate("go", false, || panic!("online never probes")), None);
        let message = GrammarError::NotCached("go".into()).to_string();
        assert!(
            message.contains("not downloaded") && message.contains("POLY_OFFLINE"),
            "{message}"
        );
        assert!(!message.contains("unknown"), "{message}");
    }

    #[test]
    fn not_cached_is_remembered_for_the_process() {
        let memo = Memo::<u32>::new();
        let t0 = Instant::now();
        let _ = memo.get("go", t0, || Err(GrammarError::NotCached("go".into())));
        assert_eq!(
            memo.get("go", t0 + RETRY_AFTER * 100, || panic!("no retry")),
            Err(GrammarError::NotCached("go".into()))
        );
    }

    #[test]
    fn a_hit_does_not_take_the_slot_mutex() {
        let memo = Memo::<u32>::new();
        let t0 = Instant::now();
        assert_eq!(memo.get("go", t0, || Ok(7)), Ok(7));
        let slot = memo.slot("go");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            // Dropped on unwind too, so a failure panics instead of hanging the join.
            let _held = slot.state.lock().unwrap();
            scope.spawn(|| tx.send(memo.get("go", t0, || panic!("cached"))).unwrap());
            let got = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("a cached lookup must not block on the slot mutex");
            assert_eq!(got, Ok(7));
        });
    }

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
