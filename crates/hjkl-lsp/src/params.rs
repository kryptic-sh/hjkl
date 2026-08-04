//! Builders for the `textDocument/*` notification `params` objects.
//!
//! These build a [`serde_json::Map`] by hand rather than using `json!`.
//! `json!` expands every interpolated expression to
//! `serde_json::to_value(&expr)`, which *deep-copies* the value even when the
//! expression is already owned — so a full-document sync copied the entire
//! buffer text once more on its way into the params object. Taking the text
//! by value and moving it into the map removes that copy.
//!
//! The full-document `didOpen` / full `didChange` paths go further: the
//! `*_borrowed` variants below serialize the text straight from a shared
//! `Arc<String>` (the app's `content_joined()` cache) with no owned
//! full-document `String` at all — the frame is serialized directly from the
//! struct via `Server::send_notification_borrowed`, whose counterpart on the
//! Value side is `server::rpc_envelope`.
//!
//! Key insertion order matches the `json!` literals these replaced, and the
//! borrowed structs' field order mirrors the sorted `BTreeMap` backend, so
//! the serialized bytes are identical under either `serde_json` map backend
//! (sorted `BTreeMap` by default, insertion-ordered `IndexMap` with the
//! `preserve_order` feature). The tests below assert exactly that.

use serde_json::{Map, Value};

use crate::event::TextChange;

/// Build a JSON object from `entries`, *moving* each value into the map.
fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    let mut map = Map::with_capacity(N);
    for (key, value) in entries {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

/// `textDocument/didOpen` params. Takes `text` by value — it is moved into
/// the object, not copied.
pub fn did_open(uri: &str, language_id: String, version: i32, text: String) -> Value {
    object([(
        "textDocument",
        object([
            ("uri", Value::from(uri)),
            ("languageId", Value::String(language_id)),
            ("version", Value::from(version)),
            ("text", Value::String(text)),
        ]),
    )])
}

/// `textDocument/didChange` params for full-document sync. Takes `text` by
/// value — it is moved into the object, not copied.
pub fn did_change_full(uri: &str, version: i32, text: String) -> Value {
    object([
        (
            "textDocument",
            object([("uri", Value::from(uri)), ("version", Value::from(version))]),
        ),
        (
            "contentChanges",
            Value::Array(vec![object([("text", Value::String(text))])]),
        ),
    ])
}

/// `textDocument/didChange` params for incremental sync. Each change's
/// replacement text is moved into the object, not copied.
pub fn did_change_incremental(uri: &str, version: i32, changes: Vec<TextChange>) -> Value {
    let content_changes: Vec<Value> = changes
        .into_iter()
        .map(|c| {
            object([
                (
                    "range",
                    object([
                        (
                            "start",
                            object([
                                ("line", Value::from(c.start_line)),
                                ("character", Value::from(c.start_col)),
                            ]),
                        ),
                        (
                            "end",
                            object([
                                ("line", Value::from(c.end_line)),
                                ("character", Value::from(c.end_col)),
                            ]),
                        ),
                    ]),
                ),
                ("text", Value::String(c.text)),
            ])
        })
        .collect();

    object([
        (
            "textDocument",
            object([("uri", Value::from(uri)), ("version", Value::from(version))]),
        ),
        ("contentChanges", Value::Array(content_changes)),
    ])
}

// ── Borrowed variants (full-document sync) ────────────────────────────────────

// The full-document `didOpen` / full `didChange` path serializes the buffer
// text straight from a shared `Arc<String>` (the app's `content_joined()`
// cache) instead of materializing an owned `serde_json::Value`: serde_json
// escapes a `&str` directly into the output writer with no intermediate
// full-document `String`. These structs are that borrowed path.
//
// Field declaration order mirrors the sorted-key serialization of
// `serde_json::Map` (the default `BTreeMap` backend — this workspace enables
// no `preserve_order` feature), so the wire bytes are IDENTICAL to the
// Value-based builders above; the tests below pin that.

/// Borrowed `textDocument/didOpen` params. Wire-identical to [`did_open`].
#[derive(serde::Serialize)]
pub struct DidOpen<'a> {
    #[serde(rename = "textDocument")]
    text_document: TextDocumentOpen<'a>,
}

#[derive(serde::Serialize)]
struct TextDocumentOpen<'a> {
    #[serde(rename = "languageId")]
    language_id: &'a str,
    text: &'a str,
    uri: &'a str,
    version: i32,
}

/// Borrowed `textDocument/didChange` params for full-document sync.
/// Wire-identical to [`did_change_full`].
#[derive(serde::Serialize)]
pub struct DidChangeFull<'a> {
    #[serde(rename = "contentChanges")]
    content_changes: [ContentChange<'a>; 1],
    #[serde(rename = "textDocument")]
    text_document: TextDocumentVersion<'a>,
}

#[derive(serde::Serialize)]
struct TextDocumentVersion<'a> {
    uri: &'a str,
    version: i32,
}

#[derive(serde::Serialize)]
struct ContentChange<'a> {
    text: &'a str,
}

/// Borrowed analogue of [`did_open`]: `text` is serialized from the borrow,
/// never copied into an owned `String`.
pub fn did_open_borrowed<'a>(
    uri: &'a str,
    language_id: &'a str,
    version: i32,
    text: &'a str,
) -> DidOpen<'a> {
    DidOpen {
        text_document: TextDocumentOpen {
            uri,
            language_id,
            version,
            text,
        },
    }
}

/// Borrowed analogue of [`did_change_full`].
pub fn did_change_full_borrowed<'a>(
    uri: &'a str,
    version: i32,
    text: &'a str,
) -> DidChangeFull<'a> {
    DidChangeFull {
        text_document: TextDocumentVersion { uri, version },
        content_changes: [ContentChange { text }],
    }
}

#[cfg(test)]
mod tests {
    //! Wire-compatibility tests: every builder must serialize to exactly the
    //! same bytes as the `json!` literal it replaced. The `json!` literals
    //! here are verbatim copies of the pre-change `runtime.rs` code.

    use serde_json::json;

    use super::*;

    fn doc() -> String {
        // Deliberately awkward: multi-byte chars, escapes, newlines — all of
        // which must survive identically through both construction paths.
        "fn main() {\n    println!(\"héllo — \\u{1F600}\\t\");\n}\n".to_string()
    }

    #[test]
    fn did_open_matches_json_macro() {
        let uri = "file:///tmp/ws/src/main.rs";
        let language_id = "rust".to_string();
        let text = doc();

        let expected = json!({
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": 1,
                "text": text,
            }
        });
        let actual = did_open(uri, language_id, 1, text);

        assert_eq!(actual, expected);
        assert_eq!(
            serde_json::to_string(&actual).unwrap(),
            serde_json::to_string(&expected).unwrap(),
        );
    }

    #[test]
    fn did_change_full_matches_json_macro() {
        let uri = "file:///tmp/ws/src/main.rs";
        let version: i32 = 42;
        let text = doc();

        let expected = json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": text }],
        });
        let actual = did_change_full(uri, version, text);

        assert_eq!(actual, expected);
        assert_eq!(
            serde_json::to_string(&actual).unwrap(),
            serde_json::to_string(&expected).unwrap(),
        );
    }

    /// The borrowed `didOpen` builder must serialize byte-identically to the
    /// Value-based one — the whole point of the borrowed path is that the
    /// frame bytes cannot change.
    #[test]
    fn borrowed_did_open_matches_value_builder_bytes() {
        let uri = "file:///tmp/ws/src/main.rs";
        let language_id = "rust";
        let version = 1;
        let text = doc();

        let value = did_open(uri, language_id.to_string(), version, text.clone());
        let borrowed = did_open_borrowed(uri, language_id, version, &text);
        assert_eq!(
            serde_json::to_string(&borrowed).unwrap(),
            serde_json::to_string(&value).unwrap(),
            "borrowed didOpen must serialize byte-identically to the Value builder"
        );
        // And the canonical json! shape directly (text with escapes/newlines).
        let expected = json!({
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": version,
                "text": text,
            }
        });
        assert_eq!(
            serde_json::to_string(&borrowed).unwrap(),
            serde_json::to_string(&expected).unwrap(),
        );
    }

    /// The borrowed full-didChange builder must serialize byte-identically to
    /// the Value-based one.
    #[test]
    fn borrowed_did_change_full_matches_value_builder_bytes() {
        let uri = "file:///tmp/ws/src/main.rs";
        let version: i32 = 42;
        let text = doc();

        let value = did_change_full(uri, version, text.clone());
        let borrowed = did_change_full_borrowed(uri, version, &text);
        assert_eq!(
            serde_json::to_string(&borrowed).unwrap(),
            serde_json::to_string(&value).unwrap(),
            "borrowed didChangeFull must serialize byte-identically to the Value builder"
        );
        let expected = json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": text }],
        });
        assert_eq!(
            serde_json::to_string(&borrowed).unwrap(),
            serde_json::to_string(&expected).unwrap(),
        );
    }

    #[test]
    fn did_change_incremental_matches_json_macro() {
        let uri = "file:///tmp/ws/src/main.rs";
        let version: i32 = 7;
        let changes = vec![
            TextChange {
                start_line: 0,
                start_col: 4,
                end_line: 0,
                end_col: 9,
                text: doc(),
            },
            TextChange {
                start_line: 12,
                start_col: 0,
                end_line: 13,
                end_col: 0,
                text: String::new(),
            },
        ];

        let expected = json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": changes.iter().map(|c| json!({
                "range": {
                    "start": { "line": c.start_line, "character": c.start_col },
                    "end":   { "line": c.end_line,   "character": c.end_col },
                },
                "text": c.text,
            })).collect::<Vec<_>>(),
        });
        let actual = did_change_incremental(uri, version, changes);

        assert_eq!(actual, expected);
        assert_eq!(
            serde_json::to_string(&actual).unwrap(),
            serde_json::to_string(&expected).unwrap(),
        );
    }

    /// The whole point of the rewrite: the document text must arrive in the
    /// params object by *move*, so the `String`'s heap buffer pointer is
    /// unchanged. `json!` would have allocated a fresh copy.
    #[test]
    fn document_text_is_moved_not_copied() {
        let text = "x".repeat(64 * 1024);
        let ptr = text.as_ptr();

        let params = did_change_full("file:///tmp/a.rs", 1, text);
        let Value::String(out) = &params["contentChanges"][0]["text"] else {
            panic!("expected a string");
        };
        assert_eq!(out.as_ptr(), ptr, "document text was copied, not moved");
    }
}
