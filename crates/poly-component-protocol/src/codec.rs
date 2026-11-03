//! MessagePack encoding of the protocol: named fields (structs as maps), enums externally tagged.

use std::io::Cursor;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Request, Response};

/// A message that could not be encoded or decoded.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// Serialization failed.
    #[error("failed to encode message: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    /// The bytes are not a valid message of the expected type.
    #[error("failed to decode message: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    /// A valid message was followed by extra bytes.
    #[error("{extra} trailing byte(s) after message")]
    TrailingBytes {
        /// How many bytes followed the message.
        extra: usize,
    },
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
    Ok(rmp_serde::to_vec_named(value)?)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ProtocolError> {
    let mut cursor = Cursor::new(bytes);
    let value = {
        let mut de = rmp_serde::Deserializer::new(&mut cursor);
        T::deserialize(&mut de)?
    };
    let consumed = usize::try_from(cursor.position()).unwrap_or(usize::MAX);
    if consumed < bytes.len() {
        return Err(ProtocolError::TrailingBytes {
            extra: bytes.len() - consumed,
        });
    }
    Ok(value)
}

/// Encode a request header.
pub fn encode_request(request: &Request) -> Result<Vec<u8>, ProtocolError> {
    encode(request)
}

/// Decode a request header. Rejects truncated input and trailing bytes.
pub fn decode_request(bytes: &[u8]) -> Result<Request, ProtocolError> {
    decode(bytes)
}

/// Encode a response.
pub fn encode_response(response: &Response) -> Result<Vec<u8>, ProtocolError> {
    encode(response)
}

/// Decode a response. Rejects truncated input and trailing bytes.
pub fn decode_response(bytes: &[u8]) -> Result<Response, ProtocolError> {
    decode(bytes)
}

/// Encode the `describe()` payload.
pub fn encode_describe(describe: &crate::Describe) -> Result<Vec<u8>, ProtocolError> {
    encode(describe)
}

/// Decode the `describe()` payload.
pub fn decode_describe(bytes: &[u8]) -> Result<crate::Describe, ProtocolError> {
    decode(bytes)
}
