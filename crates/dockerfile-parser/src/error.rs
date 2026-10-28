// (C) Copyright 2019-2020 Hewlett Packard Enterprise Development LP

use pest::iterators::Pair;
use thiserror::Error;

use crate::parser::*;

/// A Dockerfile parsing error.
#[derive(Debug, Error)]
pub enum Error {
    #[error("could not parse Dockerfile: {source}")]
    /// The input is not syntactically valid.
    ParseError {
        /// The underlying pest syntax error.
        source: Box<pest::error::Error<Rule>>,
    },

    #[error("unable to parse Dockerfile: {message}")]
    /// The input parsed but contained an invalid construct.
    GenericParseError {
        /// A description of what could not be parsed.
        message: String,
    },

    #[error("error unescaping string: {message}")]
    /// A quoted string could not be unquoted.
    UnescapeError {
        /// A description of why the string could not be unquoted.
        message: String,
    },

    #[error("unable to parse Dockerfile")]
    /// The parser produced no result.
    UnknownParseError,

    #[error("could not read Dockerfile: {source}")]
    /// The input could not be read.
    ReadError {
        /// The underlying I/O error.
        #[from]
        source: std::io::Error,
    },

    #[error("could not convert instruction '{from:?}' to desired type '{to}'")]
    /// An instruction could not be converted to the requested type.
    ConversionError {
        /// The instruction that was being converted.
        from: String,
        /// The name of the requested instruction type.
        to: String,
    },

    #[error("span {start}..{end} does not select a valid range of the {len}-byte document")]
    /// A span handed to [`Splicer::splice`](crate::Splicer::splice) is reversed, out of bounds or not on a character
    /// boundary.
    InvalidSpan {
        /// The start of the offending span.
        start: usize,
        /// The end of the offending span.
        end: usize,
        /// The length of the splice buffer the span was applied to.
        len: usize,
    },
}

/// A Dockerfile parsing Result.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Helper to create an unexpected token error.
pub(crate) fn unexpected_token(record: Pair<Rule>) -> Error {
    Error::GenericParseError {
        message: format!("unexpected token {:?}", record.as_rule()),
    }
}
