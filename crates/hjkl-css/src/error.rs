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

/// Internal error type for the cssparser parser plumbing: the message of a
/// custom `cssparser::ParseError`. Not part of the public API — `parse`
/// drops every rule and declaration error, so it never leaves the crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseErrorOwned(pub String);
