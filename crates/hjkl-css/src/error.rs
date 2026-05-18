use cssparser::{BasicParseErrorKind, ParseErrorKind};
use thiserror::Error;

/// Failure modes for [`crate::parse`]. The current implementation is
/// lenient — malformed rules and declarations are dropped silently per
/// CSS spec, so a normal `parse` call never returns this. The type is
/// preserved on the public API so a future strict-mode entry point can
/// surface diagnostics without a breaking change.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ParseError {
    #[error("CSS syntax error at line {line}, col {column}: {message}")]
    Syntax {
        line: u32,
        column: u32,
        message: String,
    },
}

impl<'i> From<cssparser::ParseError<'i, ParseErrorOwned>> for ParseError {
    fn from(err: cssparser::ParseError<'i, ParseErrorOwned>) -> Self {
        let location = err.location;
        let message = match err.kind {
            ParseErrorKind::Basic(BasicParseErrorKind::UnexpectedToken(t)) => {
                format!("unexpected token: {t:?}")
            }
            ParseErrorKind::Basic(BasicParseErrorKind::EndOfInput) => {
                "unexpected end of input".to_string()
            }
            ParseErrorKind::Basic(BasicParseErrorKind::AtRuleInvalid(s)) => {
                format!("invalid @-rule: {s}")
            }
            ParseErrorKind::Basic(BasicParseErrorKind::AtRuleBodyInvalid) => {
                "invalid @-rule body".to_string()
            }
            ParseErrorKind::Basic(BasicParseErrorKind::QualifiedRuleInvalid) => {
                "invalid rule".to_string()
            }
            ParseErrorKind::Custom(c) => c.0,
        };
        Self::Syntax {
            line: location.line,
            column: location.column,
            message,
        }
    }
}

/// Internal error type for the cssparser parser plumbing. Wraps a message
/// so the `'i` lifetime stays clean. Not part of the public API — leaks
/// out only inside `cssparser::ParseError`, which we convert to
/// [`ParseError`] at the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseErrorOwned(pub String);
