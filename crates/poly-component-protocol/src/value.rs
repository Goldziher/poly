//! [`WireValue`]: the wire replacement for `toml::Value`.

use serde::{Deserialize, Serialize};

/// A dynamically typed config value. Mirrors the TOML data model so a component never links `toml`.
///
/// Tables are an ordered list of pairs rather than a map: the order is preserved exactly as the
/// sender produced it, and a non-Rust implementer needs no ordered-map type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum WireValue {
    /// A string.
    String(String),
    /// A 64-bit signed integer.
    Integer(i64),
    /// A 64-bit float.
    Float(f64),
    /// A boolean.
    Boolean(bool),
    /// A TOML datetime in its canonical string form (RFC 3339 / TOML local forms).
    Datetime(String),
    /// An array.
    Array(Vec<WireValue>),
    /// A table; keys are unique.
    Table(Vec<(String, WireValue)>),
}

/// A [`WireValue`] that cannot be turned back into a `toml::Value`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireValueError {
    /// A datetime string that is not a valid TOML datetime.
    #[error("invalid datetime `{0}`")]
    InvalidDatetime(String),
    /// A table that repeats a key.
    #[error("duplicate table key `{0}`")]
    DuplicateKey(String),
    /// A value used where a table is required (an engine's options).
    #[error("expected a table")]
    NotATable,
}

impl From<&toml::Value> for WireValue {
    fn from(value: &toml::Value) -> Self {
        match value {
            toml::Value::String(s) => WireValue::String(s.clone()),
            toml::Value::Integer(i) => WireValue::Integer(*i),
            toml::Value::Float(f) => WireValue::Float(*f),
            toml::Value::Boolean(b) => WireValue::Boolean(*b),
            toml::Value::Datetime(d) => WireValue::Datetime(d.to_string()),
            toml::Value::Array(items) => WireValue::Array(items.iter().map(WireValue::from).collect()),
            toml::Value::Table(table) => {
                WireValue::Table(table.iter().map(|(k, v)| (k.clone(), WireValue::from(v))).collect())
            }
        }
    }
}

impl From<toml::Value> for WireValue {
    fn from(value: toml::Value) -> Self {
        WireValue::from(&value)
    }
}

impl From<&toml::Table> for WireValue {
    fn from(table: &toml::Table) -> Self {
        WireValue::Table(table.iter().map(|(k, v)| (k.clone(), WireValue::from(v))).collect())
    }
}

/// Converts a [`WireValue`] back to a `toml::Value`.
///
/// A free function rather than only `TryFrom`, because `toml::Value::try_from` is also an inherent
/// method (serialize any `T`) that shadows the trait at call sites.
///
/// # Errors
///
/// [`WireValueError`] for an unparsable datetime or a repeated table key.
pub fn to_toml(value: &WireValue) -> Result<toml::Value, WireValueError> {
    Ok(match value {
        WireValue::String(s) => toml::Value::String(s.clone()),
        WireValue::Integer(i) => toml::Value::Integer(*i),
        WireValue::Float(f) => toml::Value::Float(*f),
        WireValue::Boolean(b) => toml::Value::Boolean(*b),
        WireValue::Datetime(s) => toml::Value::Datetime(
            s.parse::<toml::value::Datetime>()
                .map_err(|_| WireValueError::InvalidDatetime(s.clone()))?,
        ),
        WireValue::Array(items) => toml::Value::Array(items.iter().map(to_toml).collect::<Result<Vec<_>, _>>()?),
        WireValue::Table(pairs) => {
            let mut table = toml::Table::new();
            for (key, item) in pairs {
                if table.insert(key.clone(), to_toml(item)?).is_some() {
                    return Err(WireValueError::DuplicateKey(key.clone()));
                }
            }
            toml::Value::Table(table)
        }
    })
}

impl TryFrom<&WireValue> for toml::Value {
    type Error = WireValueError;

    fn try_from(value: &WireValue) -> Result<Self, Self::Error> {
        to_toml(value)
    }
}

impl TryFrom<WireValue> for toml::Value {
    type Error = WireValueError;

    fn try_from(value: WireValue) -> Result<Self, Self::Error> {
        to_toml(&value)
    }
}

impl TryFrom<&WireValue> for toml::Table {
    type Error = WireValueError;

    fn try_from(value: &WireValue) -> Result<Self, Self::Error> {
        match to_toml(value)? {
            toml::Value::Table(table) => Ok(table),
            _ => Err(WireValueError::NotATable),
        }
    }
}
