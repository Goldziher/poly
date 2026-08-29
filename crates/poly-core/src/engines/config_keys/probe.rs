//! Deriving an engine's recognised option keys from the serde type it
//! deserializes its whole options table into.
//!
//! A backend that hands its entire `[<kind>.<lang>.<engine>]` table to
//! `serde` has no hand-written key list to declare — the recognised set *is*
//! whatever that type accepts, and any list repeating it would drift the moment
//! the upstream crate adds a field. These two probes read the answer off the
//! type itself, so those backends cannot drift by construction.
//!
//! Both return the subset of `options`' own keys the type recognises, or `None`
//! for "could not tell" — which the caller treats as "recognise everything",
//! because a warning invented out of a parse failure is worse than a missed one.

use std::collections::BTreeSet;

use serde::de::DeserializeOwned;

/// Recognised keys of `T`, observed by deserializing `options` into it and
/// recording what `serde` ignored.
///
/// Exact — aliases, `rename`s and nested tables are all handled by `T`'s own
/// `Deserialize` impl. Only usable when `T` does **not** `#[serde(flatten)]` a
/// sub-struct: a flattened field buffers the whole map and swallows unknown
/// keys before `serde_ignored` can see them (see [`recognized_by_serialize`],
/// which covers that case).
pub(crate) fn recognized_by_deserialize<T: DeserializeOwned>(options: &toml::Table) -> Option<Vec<String>> {
    let mut ignored: BTreeSet<String> = BTreeSet::new();
    let value = toml::Value::Table(options.clone());
    let parsed: Result<T, _> = serde_ignored::deserialize(value, |path| {
        ignored.insert(path.to_string());
    });
    // A type error stops the walk part-way, so everything after it looks
    // unrecognised. Report nothing rather than a tail of false warnings; the
    // backend already logs the parse failure itself.
    parsed.ok()?;
    Some(
        options
            .keys()
            .filter(|key| !ignored.contains(key.as_str()))
            .cloned()
            .collect(),
    )
}

/// Recognised keys of `T`, established by offering each key a value whose type
/// nothing in `T` can accept and seeing whether `T` objects.
///
/// The fallback for the tiny_pretty-family formatters (malva, markup_fmt,
/// pretty_yaml, pretty_graphql), whose `FormatOptions` `#[serde(flatten)]`s its
/// `layout` and `language` sub-structs. Flattening buffers the whole map and
/// silently drops unknown keys *before* [`recognized_by_deserialize`] can
/// observe them, so the ignored-key signal is unavailable — but the type signal
/// is: a key `T` knows rejects a TOML datetime with a type error, while a key it
/// does not know is discarded without complaint.
///
/// Serializing `T::default()` instead would look simpler and be wrong: every
/// field those crates leave `None` by default (`hex_color_length`,
/// `declaration_order`, …) vanishes from the serialized form, and each one would
/// then be reported as an unknown key. A false warning is the one outcome worse
/// than a missed one.
///
/// The datetime is the poison because no field type in those configs — bool,
/// integer, string enum, `Vec<String>`, sub-struct, or an `Option` of any of
/// them — deserializes from one.
pub(crate) fn recognized_by_type_probe<T: DeserializeOwned>(options: &toml::Table) -> Option<Vec<String>> {
    let poison: toml::Value = POISON.parse::<toml::Value>().ok()?;
    Some(
        options
            .keys()
            .filter(|key| {
                let mut probe = toml::Table::new();
                probe.insert((*key).clone(), poison.clone());
                toml::Value::Table(probe).try_into::<T>().is_err()
            })
            .cloned()
            .collect(),
    )
}

/// A TOML datetime literal, parsed once per probe. See
/// [`recognized_by_type_probe`].
const POISON: &str = "1979-05-27T07:32:00Z";
