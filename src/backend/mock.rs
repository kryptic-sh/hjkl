//! [`MockBackend`] — in-memory `Backend` for testing.
//!
//! Configurable [`BackendKind`] + [`Capabilities`] so a single mock can
//! impersonate any platform backend (e.g. capability-test code paths that
//! depend on `OSC52`-shaped capabilities without spinning a terminal).
//!
//! Records every `set` / `clear` call, returns programmable `get` /
//! `available` responses. Implements both the sync and async surface so
//! callers can exercise either path.
//!
//! # Example
//!
//! ```
//! use hjkl_clipboard::{Clipboard, MimeType, Selection, BackendKind, Capabilities};
//! use hjkl_clipboard::backend::mock::MockBackend;
//!
//! let mock = MockBackend::new(BackendKind::Mock, Capabilities::all());
//! mock.preset_get(Selection::Clipboard, MimeType::Text, Ok(b"hello".to_vec()));
//!
//! let cb = Clipboard::with_backend(Box::new(mock));
//! cb.set(Selection::Clipboard, MimeType::Text, b"world").unwrap();
//! let got = cb.get(Selection::Clipboard, MimeType::Text).unwrap();
//! assert_eq!(got, b"hello");
//! ```

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::{Backend, BackendKind, Capabilities, ClipboardError, MimeType, Selection};

type GetEntry = ((Selection, MimeType), Result<Vec<u8>, ClipboardError>);
type AvailableEntry = (Selection, Result<Vec<MimeType>, ClipboardError>);

/// Recorded `set` invocation.
#[derive(Debug, Clone, PartialEq)]
pub struct SetCall {
    pub sel: Selection,
    pub mime: MimeType,
    pub bytes: Vec<u8>,
}

#[derive(Default)]
struct State {
    sets: Vec<SetCall>,
    clears: Vec<Selection>,
    /// Last write wins (linear lookup; tests rarely program more than a few).
    gets: Vec<GetEntry>,
    availables: Vec<AvailableEntry>,
}

fn upsert_get(
    slots: &mut Vec<GetEntry>,
    key: (Selection, MimeType),
    v: Result<Vec<u8>, ClipboardError>,
) {
    if let Some(slot) = slots.iter_mut().find(|(k, _)| *k == key) {
        slot.1 = v;
    } else {
        slots.push((key, v));
    }
}

fn upsert_available(
    slots: &mut Vec<AvailableEntry>,
    key: Selection,
    v: Result<Vec<MimeType>, ClipboardError>,
) {
    if let Some(slot) = slots.iter_mut().find(|(k, _)| *k == key) {
        slot.1 = v;
    } else {
        slots.push((key, v));
    }
}

/// In-memory `Backend` for unit tests.
///
/// `Clone` shares the same recording state (uses `Arc<Mutex>` internally), so
/// the test that constructs the mock and the `Clipboard` that owns it can
/// both observe + assert.
pub struct MockBackend {
    kind: BackendKind,
    caps: Capabilities,
    state: Arc<Mutex<State>>,
}

impl MockBackend {
    /// Construct a mock advertising the given `kind` + `capabilities`.
    ///
    /// The mock honors all configured capabilities — but capability flags are
    /// only metadata; nothing prevents callers from invoking methods the mock
    /// claims not to support. Use [`Capabilities::all`] to allow everything.
    pub fn new(kind: BackendKind, caps: Capabilities) -> Self {
        Self {
            kind,
            caps,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// Get a clonable handle to the recording state.
    ///
    /// Useful when the mock is moved into `Clipboard::with_backend(Box::new(mock))`
    /// and the test still needs to observe recorded calls.
    pub fn handle(&self) -> MockHandle {
        MockHandle {
            state: Arc::clone(&self.state),
        }
    }

    /// Pre-program a response for `get(sel, mime)`. Subsequent calls return
    /// the supplied `Result` (cloned each call).
    pub fn preset_get(
        &self,
        sel: Selection,
        mime: MimeType,
        response: Result<Vec<u8>, ClipboardError>,
    ) {
        upsert_get(&mut self.state.lock().unwrap().gets, (sel, mime), response);
    }

    /// Pre-program a response for `available(sel)`.
    pub fn preset_available(
        &self,
        sel: Selection,
        response: Result<Vec<MimeType>, ClipboardError>,
    ) {
        upsert_available(&mut self.state.lock().unwrap().availables, sel, response);
    }
}

/// Read-side handle to a [`MockBackend`]'s recorded state.
///
/// Returned by [`MockBackend::handle`]. Holds an `Arc` to the same shared
/// state, so observation works after the mock is wrapped in
/// `Box<dyn Backend>` and moved into a `Clipboard`.
#[derive(Clone)]
pub struct MockHandle {
    state: Arc<Mutex<State>>,
}

impl MockHandle {
    /// All recorded `set` calls in chronological order.
    pub fn set_calls(&self) -> Vec<SetCall> {
        self.state.lock().unwrap().sets.clone()
    }

    /// All recorded `clear` calls in chronological order.
    pub fn clear_calls(&self) -> Vec<Selection> {
        self.state.lock().unwrap().clears.clone()
    }
}

#[async_trait]
impl Backend for MockBackend {
    fn kind(&self) -> BackendKind {
        self.kind
    }

    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    fn set(&self, sel: Selection, mime: MimeType, bytes: &[u8]) -> Result<(), ClipboardError> {
        self.state.lock().unwrap().sets.push(SetCall {
            sel,
            mime,
            bytes: bytes.to_vec(),
        });
        Ok(())
    }

    fn get(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        let key = (sel, mime);
        self.state
            .lock()
            .unwrap()
            .gets
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or(Err(ClipboardError::UnsupportedMime))
    }

    fn clear(&self, sel: Selection) -> Result<(), ClipboardError> {
        self.state.lock().unwrap().clears.push(sel);
        Ok(())
    }

    fn available(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        self.state
            .lock()
            .unwrap()
            .availables
            .iter()
            .find(|(k, _)| *k == sel)
            .map(|(_, v)| v.clone())
            .unwrap_or(Ok(Vec::new()))
    }

    async fn set_async(
        &self,
        sel: Selection,
        mime: MimeType,
        bytes: Vec<u8>,
    ) -> Result<(), ClipboardError> {
        self.set(sel, mime, &bytes)
    }

    async fn get_async(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        self.get(sel, mime)
    }

    async fn clear_async(&self, sel: Selection) -> Result<(), ClipboardError> {
        self.clear(sel)
    }

    async fn available_async(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        self.available(sel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Clipboard;

    #[test]
    fn records_sets() {
        let mock = MockBackend::new(BackendKind::Mock, Capabilities::all());
        let handle = mock.handle();
        let cb = Clipboard::with_backend(Box::new(mock));
        cb.set(Selection::Clipboard, MimeType::Text, b"hi").unwrap();
        cb.set(Selection::Primary, MimeType::Html, b"<p>x</p>")
            .unwrap();
        let calls = handle.set_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].bytes, b"hi");
        assert_eq!(calls[1].mime, MimeType::Html);
    }

    #[test]
    fn preset_get_returns_canned_response() {
        let mock = MockBackend::new(BackendKind::Mock, Capabilities::READ);
        mock.preset_get(Selection::Clipboard, MimeType::Text, Ok(b"canned".to_vec()));
        let cb = Clipboard::with_backend(Box::new(mock));
        let got = cb.get(Selection::Clipboard, MimeType::Text).unwrap();
        assert_eq!(got, b"canned");
    }

    #[test]
    fn unprogrammed_get_returns_unsupported() {
        let mock = MockBackend::new(BackendKind::Mock, Capabilities::READ);
        let cb = Clipboard::with_backend(Box::new(mock));
        let err = cb.get(Selection::Clipboard, MimeType::Text).unwrap_err();
        assert!(matches!(err, ClipboardError::UnsupportedMime));
    }

    #[test]
    fn kind_and_capabilities_round_trip() {
        let mock = MockBackend::new(
            BackendKind::Osc52,
            Capabilities::WRITE | Capabilities::CLEAR,
        );
        let cb = Clipboard::with_backend(Box::new(mock));
        assert_eq!(cb.kind(), BackendKind::Osc52);
        assert_eq!(cb.capabilities(), Capabilities::WRITE | Capabilities::CLEAR);
    }
}
