//! Language-neutral wire protocol between the poly host and engine components.
//!
//! A component exposes `describe() -> bytes` and `call(header, body) -> bytes`. The header is a
//! MessagePack-encoded [`Request`] (structs as maps, see [`encode_request`]); file content travels
//! in the separate `body` slice and requests carry offsets and lengths into it. The reply is a
//! MessagePack-encoded [`Response`]. A JSON Schema of every message is committed at
//! `schemas/component-protocol.json` for non-Rust implementers ([`json_schema`]).
//!
//! This crate deliberately has no dependency on `poly-core`; conversions from engine types live
//! with those types.

mod codec;
pub mod coverage;
mod value;
mod wire;

pub use codec::{
    ProtocolError, decode_describe, decode_request, decode_response, encode_describe, encode_request, encode_response,
};
pub use coverage::{Coverage, ENGINE_METHODS};
pub use value::{WireValue, WireValueError, to_toml};
pub use wire::*;

/// Version of this protocol; bumped on any incompatible change to the messages.
pub const PROTOCOL_VERSION: u32 = 1;

/// The JSON Schema of [`ProtocolSchema`], pretty-printed with a trailing newline.
pub fn json_schema() -> String {
    let schema = schemars::schema_for!(ProtocolSchema);
    let mut out = serde_json::to_string_pretty(&schema).expect("a schema is always serializable");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests;
