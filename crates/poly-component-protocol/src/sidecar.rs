//! How the two protocol calls travel over alef's framed stdio transport.
//!
//! A sidecar component is an executable the host keeps alive and talks to through
//! `LoadedComponent::invoke_bytes(method, payload)`, which carries one opaque byte payload per call
//! and one back. The protocol's own two calls map onto it:
//!
//! - `describe()` is `invoke_bytes("describe", [])`; the answer is the encoded
//!   [`Describe`](crate::Describe).
//! - `call(header, body)` is `invoke_bytes("call", pack_call(header, body))`; the answer is the
//!   encoded [`Response`](crate::Response).
//!
//! [`pack_call`] writes the header's length as a little-endian `u32`, then the header (the
//! MessagePack [`Request`](crate::Request)), then the body (the file content, the rest of the
//! payload). A producer that cannot split a payload (shorter than four bytes, or a header length
//! past its end) answers with a non-zero stdio status, which leaves the connection usable. Every
//! failure of the call itself, as opposed to the engine, is a [`Response`](crate::Response)
//! carrying a [`WireError`](crate::WireError) with a zero status, exactly as over a dylib.

/// The `invoke_bytes` method that carries `describe()`.
pub const METHOD_DESCRIBE: &str = "describe";

/// The `invoke_bytes` method that carries `call(header, body)`.
pub const METHOD_CALL: &str = "call";

/// The contract name a poly engine component provides, as declared in `alef.toml`.
pub const CONTRACT: &str = "poly";

/// A `call` payload that cannot be split into a header and a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
    /// Fewer than the four bytes of the header length.
    #[error("call payload is shorter than its 4-byte header length")]
    TooShort,
    /// The header length points past the end of the payload.
    #[error("call header length {declared} exceeds the {available} bytes that follow")]
    HeaderOverrun {
        /// The declared header length.
        declared: u32,
        /// The bytes after the length prefix.
        available: usize,
    },
    /// The header alone does not fit the `u32` length prefix.
    #[error("call header is {0} bytes, which does not fit a u32 length")]
    HeaderTooLarge(usize),
}

/// Pack a `call(header, body)` into one `invoke_bytes` payload.
///
/// # Errors
///
/// [`PayloadError::HeaderTooLarge`] when `header` is longer than `u32::MAX` bytes.
pub fn pack_call(header: &[u8], body: &[u8]) -> Result<Vec<u8>, PayloadError> {
    let length = u32::try_from(header.len()).map_err(|_| PayloadError::HeaderTooLarge(header.len()))?;
    let mut payload = Vec::with_capacity(4 + header.len() + body.len());
    payload.extend_from_slice(&length.to_le_bytes());
    payload.extend_from_slice(header);
    payload.extend_from_slice(body);
    Ok(payload)
}

/// Split a payload made by [`pack_call`] into its header and body.
///
/// # Errors
///
/// [`PayloadError`] when the payload is shorter than its declared header.
pub fn unpack_call(payload: &[u8]) -> Result<(&[u8], &[u8]), PayloadError> {
    let (prefix, rest) = payload.split_first_chunk::<4>().ok_or(PayloadError::TooShort)?;
    let declared = u32::from_le_bytes(*prefix);
    let length = usize::try_from(declared).unwrap_or(usize::MAX);
    if length > rest.len() {
        return Err(PayloadError::HeaderOverrun {
            declared,
            available: rest.len(),
        });
    }
    Ok(rest.split_at(length))
}
