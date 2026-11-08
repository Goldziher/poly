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
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tree_sitter_language_pack::Language;

/// How long a failed download is remembered before the next attempt.
pub const RETRY_AFTER: Duration = Duration::from_secs(30);

/// Why a grammar could not be provided.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrammarError {
    /// The pack has no grammar of this name (also: not cached while offline).
    /// Permanent for the process.
    #[error("unknown grammar '{0}'")]
    Unknown(String),
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
enum State<T> {
    #[default]
    Empty,
    Found(T),
    Unknown,
    Failed {
        at: Instant,
        reason: String,
    },
}

/// Per-name memo with the success/unknown/transient-failure semantics above.
/// Generic over the value so the semantics are testable without a grammar.
struct Memo<T> {
    slots: Mutex<HashMap<String, Arc<Mutex<State<T>>>>>,
}

impl<T: Clone> Memo<T> {
    fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }

    fn get(
        &self,
        name: &str,
        now: Instant,
        resolve: impl FnOnce() -> Result<T, GrammarError>,
    ) -> Result<T, GrammarError> {
        let slot = {
            let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(slots.entry(name.to_owned()).or_default())
        };
        // Held across `resolve` so concurrent callers for one name wait for a
        // single attempt rather than each starting their own.
        let mut state = slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state {
            State::Found(value) => return Ok(value.clone()),
            State::Unknown => return Err(GrammarError::Unknown(name.to_owned())),
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
                *state = State::Found(value.clone());
                Ok(value)
            }
            Err(GrammarError::Unknown(n)) => {
                *state = State::Unknown;
                Err(GrammarError::Unknown(n))
            }
            Err(error @ GrammarError::Unavailable { .. }) => {
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
/// without a download count: an uncached one is [`GrammarError::Unknown`].
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
    if poly_config::network::offline() && !tree_sitter_language_pack::has_parser(name) {
        return Err(GrammarError::Unknown(name.to_owned()));
    }
    tree_sitter_language_pack::get_language(name).map_err(|error| classify(name, &error))
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
