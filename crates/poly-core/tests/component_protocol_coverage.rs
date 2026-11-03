//! Every `Engine` trait method must have a wire mapping, and every `Diagnostic` must survive a trip
//! through `WireDiagnostic`.

use std::collections::BTreeSet;

use poly_component_protocol::{ENGINE_METHODS, WireDiagnostic};
use poly_core::engine::{Diagnostic, Edit, Severity, Span};
use proptest::prelude::*;

/// Method names declared inside `pub trait Engine { ... }`, scanned from source text.
fn engine_trait_methods() -> BTreeSet<String> {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine.rs")).unwrap();
    let mut lines = source.lines().skip_while(|l| !l.starts_with("pub trait Engine"));
    assert!(lines.next().is_some(), "`pub trait Engine` not found in engine.rs");
    let mut methods = BTreeSet::new();
    for line in lines {
        if line == "}" {
            break;
        }
        if let Some(rest) = line.strip_prefix("    fn ") {
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            methods.insert(name);
        }
    }
    methods
}

#[test]
fn every_engine_method_has_a_protocol_mapping() {
    let trait_methods = engine_trait_methods();
    assert!(trait_methods.len() > 20, "scan found only {trait_methods:?}");
    let mapped: BTreeSet<String> = ENGINE_METHODS.iter().map(|(name, _)| name.to_string()).collect();
    let unmapped: Vec<_> = trait_methods.difference(&mapped).collect();
    let stale: Vec<_> = mapped.difference(&trait_methods).collect();
    assert!(
        unmapped.is_empty(),
        "Engine methods with no entry in poly_component_protocol::ENGINE_METHODS: {unmapped:?}"
    );
    assert!(
        stale.is_empty(),
        "ENGINE_METHODS entries that are not Engine methods: {stale:?}"
    );
}

fn arb_opt_string() -> impl Strategy<Value = Option<String>> {
    prop::option::of(any::<String>())
}

fn arb_diagnostic() -> impl Strategy<Value = Diagnostic> {
    let severity = prop::sample::select(vec![Severity::Error, Severity::Warning, Severity::Info, Severity::Hint]);
    let span = prop::option::of(
        (any::<u32>(), any::<u32>(), any::<u32>(), any::<u32>()).prop_map(|(a, b, c, d)| Span {
            start_line: a,
            start_col: b,
            end_line: c,
            end_col: d,
        }),
    );
    let edit = (any::<u32>(), any::<u32>(), any::<String>()).prop_map(|(s, e, replacement)| Edit {
        start_byte: s as usize,
        end_byte: e as usize,
        replacement,
    });
    (
        any::<String>(),
        arb_opt_string(),
        severity,
        any::<String>(),
        arb_opt_string(),
        span,
        arb_opt_string(),
        prop::collection::vec(edit, 0..4),
        prop::collection::btree_map(any::<String>(), any::<String>(), 0..4),
    )
        .prop_map(
            |(engine, code, severity, title, description, span, url, fix, metadata)| Diagnostic {
                engine,
                code,
                severity,
                title,
                description,
                span,
                url,
                fix,
                metadata,
            },
        )
}

proptest! {
    #[test]
    fn diagnostic_round_trips_through_wire_diagnostic(d in arb_diagnostic()) {
        let wire = WireDiagnostic::from(&d);
        let back = Diagnostic::try_from(wire).unwrap();
        prop_assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&d).unwrap());
        // `skip_serializing_if` hides empty fix/metadata and None options in JSON; compare them directly too.
        prop_assert_eq!(back.fix.len(), d.fix.len());
        prop_assert_eq!(back.metadata, d.metadata);
        prop_assert_eq!(back.span, d.span);
        prop_assert_eq!(back.code, d.code);
    }
}

#[test]
fn unset_optionals_stay_present_on_the_wire() {
    let d = Diagnostic {
        engine: "e".into(),
        code: None,
        severity: Severity::Hint,
        title: "t".into(),
        description: None,
        span: None,
        url: None,
        fix: vec![],
        metadata: Default::default(),
    };
    let wire = WireDiagnostic::from(&d);
    let bytes = rmp_serde_probe(&wire);
    for field in ["code", "description", "span", "url", "fix", "metadata"] {
        assert!(bytes.contains(field), "field `{field}` missing from {bytes:?}");
    }
}

/// The diagnostic as a response, rendered lossily so field names can be searched for.
fn rmp_serde_probe(wire: &WireDiagnostic) -> String {
    use poly_component_protocol::{Payload, Response, encode_response};
    let response = Response {
        result: Ok(Payload::Diagnostics(vec![wire.clone()])),
        logs: vec![],
    };
    String::from_utf8_lossy(&encode_response(&response).unwrap()).into_owned()
}
