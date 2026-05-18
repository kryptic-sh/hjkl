/// One styled byte range on a buffer row.
///
/// Style spans are opaque-id tuples. The buffer does not own colors. The
/// host (engine layer or terminal frontend) keeps the table mapping
/// `style: u32` to a renderable type.
///
/// Byte ranges are **half-open**: `[start_byte, end_byte)`. They line up
/// with the row's `String` so callers can slice without re-deriving
/// indices.
///
/// The host installs spans via the engine's `Editor::install_syntax_spans`
/// (or equivalent) after each edit. Spans are cleared for affected rows on
/// every [`crate::Buffer::apply_edit`] call and must be re-installed by the
/// host's syntax pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start_byte: usize,
    pub end_byte: usize,
    /// Opaque style id resolved by the host's render layer.
    pub style: u32,
}

impl Span {
    pub const fn new(start_byte: usize, end_byte: usize, style: u32) -> Self {
        Self {
            start_byte,
            end_byte,
            style,
        }
    }

    /// Width of the span in bytes; useful for render-cache fingerprints.
    pub const fn len(self) -> usize {
        self.end_byte.saturating_sub(self.start_byte)
    }

    pub const fn is_empty(self) -> bool {
        self.end_byte <= self.start_byte
    }
}

#[cfg(test)]
mod tests {
    use super::Span;

    #[test]
    fn len_and_is_empty() {
        assert_eq!(Span::new(0, 5, 0).len(), 5);
        assert!(Span::new(3, 3, 0).is_empty());
        assert!(Span::new(7, 5, 0).is_empty());
    }
}
