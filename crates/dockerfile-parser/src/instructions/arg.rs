// (C) Copyright 2019-2020 Hewlett Packard Enterprise Development LP

use std::convert::TryFrom;

use crate::SpannedString;
use crate::dockerfile_parser::Instruction;
use crate::error::*;
use crate::parse_string;
use crate::parser::{Pair, Rule};
use crate::splicer::Span;

/// A Dockerfile [`ARG` instruction][arg].
///
/// An `ARG` line declaring several arguments (`ARG A=1 B`) yields one `ArgInstruction` per argument, all sharing the
/// span of the whole line.
///
/// [arg]: https://docs.docker.com/engine/reference/builder/#arg
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgInstruction {
    /// The span of this node in the original input.
    pub span: Span,

    /// The argument key
    pub name: SpannedString,

    /// An optional argument value.
    ///
    /// This may be unset when passing arguments through to later stages in a
    /// [multi-stage build][build].
    ///
    /// [build]: https://docs.docker.com/develop/develop-images/multistage-build/
    pub value: Option<SpannedString>,
}

impl ArgInstruction {
    /// Parses a single `arg_pair` (`NAME` or `NAME=value`) belonging to an instruction spanning `span`.
    fn from_pair(span: Span, pair: Pair) -> Result<ArgInstruction> {
        let mut name = None;
        let mut value = None;

        for field in pair.into_inner() {
            match field.as_rule() {
                Rule::arg_name => name = Some(parse_string(&field)?),
                Rule::arg_quoted_value | Rule::arg_value => value = Some(parse_string(&field)?),
                Rule::comment => continue,
                _ => return Err(unexpected_token(field)),
            }
        }

        let name = name.ok_or_else(|| Error::GenericParseError {
            message: "arg name is required".into(),
        })?;

        Ok(ArgInstruction { span, name, value })
    }

    /// Parses every argument of an `ARG` instruction.
    ///
    /// `ARG A=1 B` declares two arguments; each becomes its own `ArgInstruction` sharing the span of the whole line.
    pub(crate) fn all_from_record(record: Pair) -> Result<Vec<ArgInstruction>> {
        let span = Span::from_pair(&record);
        let mut args = Vec::new();

        for field in record.into_inner() {
            match field.as_rule() {
                Rule::arg_pair => args.push(ArgInstruction::from_pair(span, field)?),
                Rule::comment => continue,
                _ => return Err(unexpected_token(field)),
            }
        }

        if args.is_empty() {
            return Err(Error::GenericParseError {
                message: "arg name is required".into(),
            });
        }

        Ok(args)
    }

    /// Parses the first argument of an `ARG` instruction.
    pub(crate) fn from_record(record: Pair) -> Result<ArgInstruction> {
        let mut args = ArgInstruction::all_from_record(record)?;
        Ok(args.swap_remove(0))
    }
}

impl<'a> TryFrom<&'a Instruction> for &'a ArgInstruction {
    type Error = Error;

    fn try_from(instruction: &'a Instruction) -> std::result::Result<Self, Self::Error> {
        if let Instruction::Arg(a) = instruction {
            Ok(a)
        } else {
            Err(Error::ConversionError {
                from: format!("{:?}", instruction),
                to: "ArgInstruction".into(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::Dockerfile;
    use crate::test_util::*;

    #[test]
    fn arg_strings() -> Result<()> {
        assert_eq!(
            parse_single(r#"arg foo=bar"#, Rule::arg)?,
            ArgInstruction {
                span: Span::new(0, 11),
                name: SpannedString {
                    span: Span::new(4, 7),
                    content: "foo".into(),
                },
                value: Some(SpannedString {
                    span: Span::new(8, 11),
                    content: "bar".into(),
                }),
            }
            .into()
        );

        assert_eq!(
            parse_single(r#"arg foo="bar""#, Rule::arg)?,
            ArgInstruction {
                span: Span::new(0, 13),
                name: SpannedString {
                    span: Span::new(4, 7),
                    content: "foo".into(),
                },
                value: Some(SpannedString {
                    span: Span::new(8, 13),
                    content: "bar".into(),
                }),
            }
            .into()
        );

        assert_eq!(
            parse_single(r#"arg foo='bar'"#, Rule::arg)?,
            ArgInstruction {
                span: Span::new(0, 13),
                name: SpannedString {
                    span: Span::new(4, 7),
                    content: "foo".into(),
                },
                value: Some(SpannedString {
                    span: Span::new(8, 13),
                    content: "bar".into(),
                }),
            }
            .into()
        );

        assert!(Dockerfile::parse(r#"arg foo="bar"bar"#).is_err());
        assert!(Dockerfile::parse(r#"arg foo='bar'bar"#).is_err());

        Ok(())
    }
}
