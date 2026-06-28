//! String-or-array value types shared across the `[hooks]` schema.
//!
//! [`Patterns`] (issue #2250) deserializes from either a single string or an
//! array of strings, so `files = "src/**"` and `files = ["a", "b"]` are both
//! valid. It backs file globs (`files`/`exclude`/`glob`) and the
//! `before`/`after` command lists, whose on-disk shape is identical.
//!
//! [`Guard`] backs the lefthook-style `skip`/`only` keys: either a bare boolean
//! (`skip = true`) or a list of conditions (`skip = ["merge", { ref = "main" }]`).

use serde::{Deserialize, Deserializer};

/// A list of strings that accepts either a single string or an array of
/// strings on the wire.
///
/// ```toml
/// files = "src/**/*.rs"          # single
/// exclude = ["a/**", "b/**"]     # array
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Patterns(Vec<String>);

impl Patterns {
    /// Borrow the underlying strings.
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    /// Iterate over the strings.
    pub fn iter(&self) -> std::slice::Iter<'_, String> {
        self.0.iter()
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl From<Vec<String>> for Patterns {
    fn from(values: Vec<String>) -> Self {
        Patterns(values)
    }
}

impl<'a> IntoIterator for &'a Patterns {
    type Item = &'a String;
    type IntoIter = std::slice::Iter<'a, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// On-disk shape of [`Patterns`]: a bare string or an array of strings.
#[derive(Deserialize)]
#[serde(untagged)]
enum PatternsRepr {
    One(String),
    Many(Vec<String>),
}

impl<'de> Deserialize<'de> for Patterns {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match PatternsRepr::deserialize(deserializer)? {
            PatternsRepr::One(single) => Patterns(vec![single]),
            PatternsRepr::Many(many) => Patterns(many),
        })
    }
}

/// A lefthook-style `skip`/`only` guard.
///
/// Either an unconditional boolean or a list of conditions; if any condition
/// matches, the guard is considered active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guard {
    /// `skip = true` / `only = false` — unconditional.
    Always(bool),
    /// A list of git-operation names and/or `{ ref, run }` match conditions.
    Conditions(Vec<GuardCondition>),
}

/// One entry in a [`Guard::Conditions`] list.
///
/// Pragmatic subset of lefthook's model: either a bare git-operation name
/// (`"merge"`, `"rebase"`) / branch name, or a `{ ref = "...", run = "..." }`
/// match table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum GuardCondition {
    /// A git operation or branch name (e.g. `"merge"`, `"rebase"`).
    Operation(String),
    /// A `{ ref, run }` match table.
    Match(GuardMatch),
}

/// A `{ ref = "...", run = "..." }` guard match table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GuardMatch {
    /// Branch-ref glob the guard applies to (lefthook `ref`).
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// Shell command whose exit status decides the guard (lefthook `run`).
    pub run: Option<String>,
}

/// On-disk shape of [`Guard`]: a bare bool or a list of conditions.
#[derive(Deserialize)]
#[serde(untagged)]
enum GuardRepr {
    Bool(bool),
    Conditions(Vec<GuardCondition>),
}

impl<'de> Deserialize<'de> for Guard {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match GuardRepr::deserialize(deserializer)? {
            GuardRepr::Bool(value) => Guard::Always(value),
            GuardRepr::Conditions(conditions) => Guard::Conditions(conditions),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Holder {
        patterns: Patterns,
    }

    #[test]
    fn patterns_accepts_single_string() {
        let holder: Holder = toml::from_str(r#"patterns = "src/**/*.rs""#).unwrap();
        assert_eq!(holder.patterns.as_slice(), &["src/**/*.rs".to_string()]);
    }

    #[test]
    fn patterns_accepts_array() {
        let holder: Holder = toml::from_str(r#"patterns = ["a/**", "b/**"]"#).unwrap();
        assert_eq!(
            holder.patterns.as_slice(),
            &["a/**".to_string(), "b/**".to_string()]
        );
        assert_eq!(holder.patterns.len(), 2);
        assert!(!holder.patterns.is_empty());
    }

    #[derive(Deserialize)]
    struct GuardHolder {
        skip: Guard,
    }

    #[test]
    fn guard_accepts_bare_bool() {
        let holder: GuardHolder = toml::from_str("skip = true").unwrap();
        assert_eq!(holder.skip, Guard::Always(true));
    }

    #[test]
    fn guard_accepts_condition_list() {
        let holder: GuardHolder = toml::from_str(r#"skip = ["merge", { ref = "main" }]"#).unwrap();
        match holder.skip {
            Guard::Conditions(conditions) => {
                assert_eq!(conditions.len(), 2);
                assert_eq!(
                    conditions[0],
                    GuardCondition::Operation("merge".to_string())
                );
                assert_eq!(
                    conditions[1],
                    GuardCondition::Match(GuardMatch {
                        reference: Some("main".to_string()),
                        run: None,
                    })
                );
            }
            other => panic!("expected conditions, got {other:?}"),
        }
    }
}
