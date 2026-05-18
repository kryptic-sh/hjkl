//! [`MimeType`] — clipboard content type discriminant.

/// The content type of a clipboard payload.
///
/// This enum is `#[non_exhaustive]`: new variants may be added in future minor
/// versions without a breaking change. Use [`MimeType::Custom`] as an escape
/// hatch for types not yet listed here.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MimeType {
    /// Plain UTF-8 text (`text/plain;charset=utf-8`).
    Text,
    /// HTML markup (`text/html`).
    Html,
    /// Rich Text Format (`text/rtf`).
    Rtf,
    /// Newline-delimited URI list (`text/uri-list`).
    UriList,
    /// PNG image (`image/png`).
    Png,
    /// Raw passthrough — no translation performed by the backend.
    Custom(String),
}
