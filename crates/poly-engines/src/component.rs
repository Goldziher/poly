//! The component contract: the two calls a host makes on a loaded engine component.

use std::error::Error;

/// A failure from a component call: a message and, optionally, the chain of causes behind it.
///
/// Built from a bare `String` for the common case of a component reporting its own error text.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ComponentCallError {
    message: String,
    #[source]
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ComponentCallError {
    /// An error with `message` and no underlying cause.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// An error with `message` caused by `source`; the cause stays reachable through
    /// [`Error::source`].
    pub fn with_source(message: impl Into<String>, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    /// The error's own message, without its causes.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<String> for ComponentCallError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

/// The contract between the host and an engine component.
///
/// Both calls exchange opaque, already-encoded byte buffers; the framing of `header` and `body`
/// belongs to the component protocol, not to this trait. `Send + Sync` because the host calls a
/// component from its rayon worker pool.
pub trait PolyComponent: Send + Sync {
    /// The component's self-description (engines, languages, versions), encoded for the host.
    fn describe(&self) -> Result<Vec<u8>, ComponentCallError>;

    /// One engine call: `header` names the operation and engine, `body` carries its input.
    /// Returns the encoded result.
    fn call(&self, header: &[u8], body: &[u8]) -> Result<Vec<u8>, ComponentCallError>;
}

/// The contract between the host and a frontend component (the MCP server).
///
/// A frontend hosts no engines. It is started once, takes over the process's stdio and serves its
/// client until that client leaves, doing its work through the host services the component ABI
/// hands it when it is created (see `poly_frontend_api`). Both calls exchange opaque, encoded
/// byte buffers, as [`PolyComponent`] does.
pub trait PolyFrontend: Send + Sync {
    /// The frontend's self-description (name, version), encoded for the host.
    fn describe(&self) -> Result<Vec<u8>, ComponentCallError>;

    /// Serve until the client disconnects. `args` is a JSON `FrontendArgs`
    /// (`poly_frontend_api::wire`); the reply is empty on a clean end.
    fn run(&self, args: &[u8]) -> Result<Vec<u8>, ComponentCallError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_converts_to_an_error_without_a_source() {
        let err = ComponentCallError::from("boom".to_owned());
        assert_eq!(err.to_string(), "boom");
        assert!(err.source().is_none());
    }

    #[test]
    fn a_cause_stays_reachable_through_source() {
        let err = ComponentCallError::with_source("outer", std::io::Error::other("inner"));
        assert_eq!(err.to_string(), "outer");
        assert_eq!(err.source().map(ToString::to_string).as_deref(), Some("inner"));
    }

    struct Echo;

    impl PolyComponent for Echo {
        fn describe(&self) -> Result<Vec<u8>, ComponentCallError> {
            Ok(b"echo".to_vec())
        }

        fn call(&self, header: &[u8], body: &[u8]) -> Result<Vec<u8>, ComponentCallError> {
            if header.is_empty() {
                return Err("empty header".to_owned().into());
            }
            Ok(body.to_vec())
        }
    }

    #[test]
    fn the_trait_is_object_safe() {
        let component: Box<dyn PolyComponent> = Box::new(Echo);
        assert_eq!(component.describe().unwrap(), b"echo");
        assert_eq!(component.call(b"h", b"body").unwrap(), b"body");
        assert_eq!(component.call(b"", b"x").unwrap_err().message(), "empty header");
    }
}
