//! Option keys read from the compiled component index rather than from a linked engine.
//!
//! Config validation must not depend on which engine families a binary links or which
//! components are installed, so the keys an engine declares for a table come from the index
//! ([`poly_component_index::index`]); only an engine the index does not describe (the core's own
//! backends) is asked directly. [`StaticKeys`] is the owned form both sources convert into, with
//! the same acceptance rules as [`OptionKeys`].

use poly_component_protocol::{WireOptionKeys, WireOptionTable};

use crate::engine::{
    ALWAYS_OPTION_KEYS, DerivedOptionKeys, DerivedProbe, OptionKeys, OptionTable, OptionType, RULE_SELECTION_KEYS,
    UNIVERSAL_OPTION_KEYS,
};

/// How the keys of a serde-derived table are recognised.
#[derive(Debug, Clone, Copy)]
enum Derived {
    /// The table has no derived keys.
    None,
    /// Derived keys exist but live in a component that is not consulted; every key is accepted.
    Unresolved,
    /// An in-process probe over a linked serde type.
    Local(DerivedOptionKeys),
}

/// The option keys one engine reads from one config table, in owned form.
#[derive(Debug, Clone)]
pub(crate) struct StaticKeys {
    checked: bool,
    declared: Vec<(String, OptionType)>,
    rule_selection: bool,
    derived: Derived,
    note: Option<String>,
}

/// The wire spelling of an engine option table.
pub(crate) fn wire_table(table: OptionTable) -> WireOptionTable {
    match table {
        OptionTable::Lint => WireOptionTable::Lint,
        OptionTable::Format => WireOptionTable::Format,
        OptionTable::CrossCuttingLint => WireOptionTable::CrossCuttingLint,
    }
}

impl StaticKeys {
    /// The keys the index declares for `engine`'s `table`, for `language` (`None`: any).
    pub(crate) fn from_index(engine: &str, language: Option<&str>, table: OptionTable) -> Option<StaticKeys> {
        poly_component_index::index()
            .option_keys(engine, language, wire_table(table))
            .map(StaticKeys::from_wire)
    }

    /// Convert an index declaration.
    pub(crate) fn from_wire(keys: &WireOptionKeys) -> StaticKeys {
        StaticKeys {
            checked: keys.checked,
            declared: keys
                .declared
                .iter()
                .filter_map(|key| Some((key.name.clone(), OptionType::from_bits(key.types)?)))
                .collect(),
            rule_selection: keys.rule_selection,
            derived: if keys.has_derived {
                Derived::Unresolved
            } else {
                Derived::None
            },
            note: keys.note.clone(),
        }
    }

    /// Run a derived probe in-process when the engine is linked into this binary.
    ///
    /// The index can only say that a table has derived keys; when `linked` (the same engine's
    /// declaration from this binary) carries a local probe, use it, so a bundled build still
    /// rejects a misspelled derived key. Without one the keys stay unresolved and are accepted.
    pub(crate) fn resolved_with(mut self, linked: Option<&OptionKeys>) -> StaticKeys {
        if matches!(self.derived, Derived::Unresolved)
            && let Some(DerivedProbe::Local(probe)) = linked.and_then(OptionKeys::derived_probe)
        {
            self.derived = Derived::Local(probe);
        }
        self
    }

    /// Convert a linked engine's declaration.
    pub(crate) fn from_linked(keys: &OptionKeys) -> StaticKeys {
        StaticKeys {
            checked: keys.is_checked(),
            declared: keys
                .declared_keys()
                .iter()
                .map(|(name, ty)| ((*name).to_string(), *ty))
                .collect(),
            rule_selection: keys.has_rule_selection(),
            derived: match keys.derived_probe() {
                None => Derived::None,
                Some(DerivedProbe::Local(probe)) => Derived::Local(probe),
                Some(DerivedProbe::Component { .. }) => Derived::Unresolved,
            },
            note: keys.note().map(str::to_string),
        }
    }

    /// Whether the table's keys are checked at all.
    pub(crate) fn is_checked(&self) -> bool {
        self.checked
    }

    /// Guidance to render with an unknown-key warning.
    pub(crate) fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Whether some keys of the table can only be recognised by a probe that is not consulted.
    pub(crate) fn has_unresolved_derived(&self) -> bool {
        matches!(self.derived, Derived::Unresolved)
    }

    /// Whether `key` is one this table accepts. An unresolved derived probe accepts everything.
    pub(crate) fn accepts(&self, key: &str, options: &toml::Table, universal: bool) -> bool {
        if !self.checked || self.expected_type(key, universal).is_some() {
            return true;
        }
        match self.derived {
            Derived::None => false,
            Derived::Unresolved => true,
            Derived::Local(probe) => probe(options).is_none_or(|recognized| recognized.iter().any(|k| k == key)),
        }
    }

    /// The declared type of `key`, or `None` when it is undeclared or only a derived probe knows it.
    pub(crate) fn expected_type(&self, key: &str, universal: bool) -> Option<OptionType> {
        if !self.checked {
            return None;
        }
        let static_lookup =
            |table: &[(&str, OptionType)]| table.iter().find(|(name, _)| *name == key).map(|(_, ty)| *ty);
        self.declared
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, ty)| *ty)
            .or_else(|| {
                self.rule_selection
                    .then(|| static_lookup(RULE_SELECTION_KEYS))
                    .flatten()
            })
            .or_else(|| universal.then(|| static_lookup(UNIVERSAL_OPTION_KEYS)).flatten())
            .or_else(|| static_lookup(ALWAYS_OPTION_KEYS))
    }

    /// The type `key` should have been given, when `value` is one this table cannot use.
    pub(crate) fn value_problem(&self, key: &str, value: &toml::Value, universal: bool) -> Option<OptionType> {
        let expected = self.expected_type(key, universal)?;
        if expected.accepts(value) {
            return None;
        }
        match self.derived {
            Derived::None => {}
            // The component's own type is the authority on a key it recognises; not consulted here.
            Derived::Unresolved => return None,
            Derived::Local(probe) => {
                let mut single = toml::Table::new();
                single.insert(key.to_string(), value.clone());
                if probe(&single).is_none_or(|recognized| recognized.iter().any(|k| k == key)) {
                    return None;
                }
            }
        }
        Some(expected)
    }

    /// Every key this table names, for the "keys read" hint.
    pub(crate) fn known_keys(&self, universal: bool) -> Vec<String> {
        let mut keys: Vec<String> = self.declared.iter().map(|(name, _)| name.clone()).collect();
        let extra = |table: &[(&str, OptionType)], keys: &mut Vec<String>| {
            keys.extend(table.iter().map(|(name, _)| (*name).to_string()));
        };
        if self.rule_selection {
            extra(RULE_SELECTION_KEYS, &mut keys);
        }
        if universal {
            extra(UNIVERSAL_OPTION_KEYS, &mut keys);
        }
        extra(ALWAYS_OPTION_KEYS, &mut keys);
        keys.sort_unstable();
        keys.dedup();
        keys
    }
}
