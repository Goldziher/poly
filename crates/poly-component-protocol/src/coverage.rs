//! How every `Engine` trait method crosses the wire. `poly-core`'s
//! `tests/component_protocol_coverage.rs` fails when the trait and this table disagree.

/// How an `Engine` method is served to a component host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// Carried by the named [`Request`](crate::Request) variant.
    Request(&'static str),
    /// Answered by the host from the static [`Describe`](crate::Describe) index; no call is made.
    Index(&'static str),
    /// Not a wire concern; the reason says why.
    HostSide(&'static str),
}

/// Every method of `poly_core::engine::Engine` and how it is served.
pub const ENGINE_METHODS: &[(&str, Coverage)] = &[
    ("name", Coverage::Index("WireEngineDesc.name")),
    ("languages", Coverage::Index("WireEngineDesc.languages")),
    ("capabilities", Coverage::Index("WireEngineDesc.capabilities")),
    ("option_keys", Coverage::Index("WireEngineDesc.option_keys")),
    ("version", Coverage::Index("WireEngineDesc.version")),
    ("project_check", Coverage::Request("ProjectCheck")),
    ("project_inputs", Coverage::Index("WireEngineDesc.project_inputs")),
    ("run_context", Coverage::Request("RunContext")),
    ("file_context", Coverage::Request("FileContext")),
    ("provides_language_lint", Coverage::Request("ProvidesLanguage")),
    ("provides_language_format", Coverage::Request("ProvidesLanguage")),
    ("skip_reason", Coverage::Request("SkipReason")),
    ("lint_input_salt", Coverage::Request("LintInputSalt")),
    (
        "format_config",
        Coverage::HostSide("applied inside the component's Format; its effect on the cache key is FormatInputSalt"),
    ),
    ("format_input_salt", Coverage::Request("FormatInputSalt")),
    ("lint", Coverage::Request("Lint")),
    ("format", Coverage::Request("Format")),
    ("batch_support", Coverage::Request("BatchSupport")),
    ("format_batch", Coverage::Request("FormatBatch")),
    ("lint_batch", Coverage::Request("LintBatch")),
    ("lint_cache_salt", Coverage::Request("LintCacheSalt")),
    (
        "lint_cache_version",
        Coverage::HostSide("derived by the host from version() and LintCacheSalt"),
    ),
    ("batch_shard_floor", Coverage::Index("WireEngineDesc.batch_shard_floor")),
    ("overridden_hooks", Coverage::Index("WireEngineDesc.hooks")),
    (
        "takes_over_tier_one",
        Coverage::Index("WireEngineDesc.takes_over_tier_one"),
    ),
    ("claimed_paths", Coverage::Index("WireEngineDesc.claimed_paths")),
    ("host_hook", Coverage::Index("WireEngineDesc.host_hook")),
    (
        "supplements_tier_one",
        Coverage::Index("WireEngineDesc.supplements_tier_one"),
    ),
    (
        "self_manages_enabled",
        Coverage::Index("WireEngineDesc.self_manages_enabled"),
    ),
    (
        "supersedes_generic_formatter",
        Coverage::Index("WireEngineDesc.supersedes_generic_formatter"),
    ),
];
