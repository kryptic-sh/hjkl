//! Position-encoding negotiation (LSP `general.positionEncodings`).
//!
//! The LSP spec lets client and server negotiate the unit used for the
//! `character` field of every `Position`/`Range` exchanged between them:
//! UTF-8 byte offsets, UTF-16 code units (the spec default — every server
//! MUST support it even if it never says so), or UTF-32 code points (rare,
//! unsupported here). hjkl advertises both `"utf-8"` and `"utf-16"` in
//! `general.positionEncodings` (see `crate::server::initialize_handshake`)
//! and honours whichever the server picks back in `initialize`'s
//! `capabilities.positionEncoding`.

/// The unit used for the `character` field of every `Position`/`Range`
/// exchanged with a given server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PositionEncoding {
    /// UTF-8 byte offsets within a line. hjkl's preferred encoding — skips
    /// per-line UTF-16 unit counting when the server supports it.
    Utf8,
    /// UTF-16 code units within a line. The LSP spec's default: a server
    /// that never mentions `positionEncoding` in its `initialize` response
    /// is using this.
    #[default]
    Utf16,
}

impl PositionEncoding {
    /// Parse the negotiated encoding from a server's `initialize` response
    /// `capabilities`. Absence of `positionEncoding`, or any value other
    /// than `"utf-8"`, means UTF-16 per spec — this also covers a server
    /// that (incorrectly) echoes `"utf-32"`, which hjkl doesn't implement;
    /// falling back to UTF-16 is the spec-mandated safe default rather than
    /// silently mis-converting UTF-32 offsets as UTF-16 ones.
    pub fn from_capabilities(capabilities: &serde_json::Value) -> Self {
        match capabilities
            .get("positionEncoding")
            .and_then(|v| v.as_str())
        {
            Some("utf-8") => PositionEncoding::Utf8,
            _ => PositionEncoding::Utf16,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PositionEncoding;
    use serde_json::json;

    #[test]
    fn missing_field_defaults_to_utf16() {
        assert_eq!(
            PositionEncoding::from_capabilities(&json!({})),
            PositionEncoding::Utf16
        );
    }

    #[test]
    fn explicit_utf8() {
        assert_eq!(
            PositionEncoding::from_capabilities(&json!({ "positionEncoding": "utf-8" })),
            PositionEncoding::Utf8
        );
    }

    #[test]
    fn explicit_utf16() {
        assert_eq!(
            PositionEncoding::from_capabilities(&json!({ "positionEncoding": "utf-16" })),
            PositionEncoding::Utf16
        );
    }

    #[test]
    fn unknown_value_defaults_to_utf16() {
        assert_eq!(
            PositionEncoding::from_capabilities(&json!({ "positionEncoding": "utf-32" })),
            PositionEncoding::Utf16
        );
    }

    #[test]
    fn null_capabilities_defaults_to_utf16() {
        assert_eq!(
            PositionEncoding::from_capabilities(&serde_json::Value::Null),
            PositionEncoding::Utf16
        );
    }
}
