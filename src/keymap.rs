//! The public [`Keymap`] API that consumers use for chord dispatch.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::chord::{Chord, ChordParseError};
use crate::key::KeyEvent;
use crate::trie::{Binding, TrieNode};

/// The vim mode a binding is scoped to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Normal,
    Insert,
    Visual,
    OpPending,
    CommandLine,
}

/// Error returned from [`Keymap`] operations.
#[derive(Debug, Error)]
pub enum KeymapError {
    #[error("chord parse error: {0}")]
    Parse(#[from] ChordParseError),
    #[error("chord is empty")]
    EmptyChord,
}

/// Result of feeding a key event into the keymap.
#[derive(Debug)]
pub enum KeyResolve<A> {
    /// The key extends an incomplete chord — wait for more keys.
    Pending,
    /// A terminal chord was matched.
    Match(Binding<A>),
    /// An exact terminal match exists **and** longer chords also start
    /// with this prefix. Caller waits for timeout to disambiguate.
    Ambiguous,
    /// No chord matches the buffered sequence. `Vec` contains the buffered
    /// keys (including the last one) that should be replayed to the engine.
    Unbound(Vec<KeyEvent>),
}

/// Per-mode pending-chord state.
#[derive(Default)]
struct ModeState {
    /// Buffered key events since the last resolution.
    buffer: Vec<KeyEvent>,
}

/// A modal keymap that maps chord sequences to user-defined actions.
///
/// Chords are stored per-[`Mode`] in separate tries. Call [`Keymap::feed`]
/// once per key event; it manages an internal per-mode buffer and returns
/// a [`KeyResolve`] indicating what happened.
pub struct Keymap<A> {
    trees: HashMap<Mode, TrieNode<A>>,
    leader: char,
    timeout: Duration,
    /// Per-mode chord accumulation state.
    state: HashMap<Mode, ModeState>,
}

impl<A: Clone> Keymap<A> {
    /// Create a new keymap with the given leader character.
    pub fn new(leader: char) -> Self {
        Self {
            trees: HashMap::new(),
            leader,
            timeout: Duration::from_millis(500),
            state: HashMap::new(),
        }
    }

    /// Update the leader character (re-parses are not needed; leader is
    /// applied at `add`/`feed` time through `Chord::parse`).
    pub fn set_leader(&mut self, c: char) {
        self.leader = c;
    }

    /// Override the ambiguity-resolution timeout.
    pub fn set_timeout(&mut self, t: Duration) {
        self.timeout = t;
    }

    /// The current leader character.
    pub fn leader(&self) -> char {
        self.leader
    }

    /// The current timeout duration.
    pub fn timeout_duration(&self) -> Duration {
        self.timeout
    }

    // ── Binding registration ──────────────────────────────────────────────

    /// Parse `chord_str` (vim notation, `<leader>` expanded) and register
    /// `action` for `mode`.
    pub fn add(
        &mut self,
        mode: Mode,
        chord_str: &str,
        action: A,
        desc: &str,
    ) -> Result<(), KeymapError> {
        let chord = Chord::parse(chord_str, self.leader)?;
        if chord.is_empty() {
            return Err(KeymapError::EmptyChord);
        }
        let binding = Binding {
            action,
            desc: desc.to_string(),
            recursive: false,
        };
        self.add_chord(mode, chord, binding);
        Ok(())
    }

    /// Register a pre-parsed chord + binding.
    pub fn add_chord(&mut self, mode: Mode, chord: Chord, binding: Binding<A>) {
        self.trees
            .entry(mode)
            .or_default()
            .insert(&chord.0, binding);
    }

    /// Remove the binding for `chord_str` in `mode`. Returns `Ok(true)` if
    /// something was actually removed.
    pub fn remove(&mut self, mode: Mode, chord_str: &str) -> Result<bool, KeymapError> {
        let chord = Chord::parse(chord_str, self.leader)?;
        if chord.is_empty() {
            return Err(KeymapError::EmptyChord);
        }
        let removed = self
            .trees
            .get_mut(&mode)
            .map(|t| t.remove(&chord.0))
            .unwrap_or(false);
        Ok(removed)
    }

    // ── Query API ─────────────────────────────────────────────────────────

    /// Return the direct-child terminal bindings reachable from `prefix` in
    /// `mode`. Used by which-key to list available completions.
    pub fn children(&self, mode: Mode, prefix: &Chord) -> Vec<(KeyEvent, Binding<A>)> {
        let Some(tree) = self.trees.get(&mode) else {
            return vec![];
        };
        tree.children_of(&prefix.0)
            .into_iter()
            .map(|(k, b)| (*k, b.clone()))
            .collect()
    }

    /// Return **all** direct children reachable from `prefix` in `mode` —
    /// both terminal bindings and pure-prefix (submenu) entries.
    ///
    /// Terminal entries carry `Some(Binding)`; prefix-only entries carry `None`.
    /// Callers (e.g. which-key) should render prefix-only entries with a
    /// synthetic description such as `"…"`.
    pub fn children_all(&self, mode: Mode, prefix: &Chord) -> Vec<(KeyEvent, Option<Binding<A>>)> {
        let Some(tree) = self.trees.get(&mode) else {
            return vec![];
        };
        tree.all_children_of(&prefix.0)
            .into_iter()
            .map(|(k, b)| (*k, b.cloned()))
            .collect()
    }

    // ── Stateful feed ─────────────────────────────────────────────────────

    /// Feed a single key event for `mode` and return what happened.
    ///
    /// `now` is used to drive timeout logic — pass `Instant::now()` in
    /// production; use a fake `Instant` in tests if needed.
    pub fn feed(&mut self, mode: Mode, ev: KeyEvent, _now: Instant) -> KeyResolve<A> {
        let state = self.state.entry(mode).or_default();
        state.buffer.push(ev);
        let buf = state.buffer.clone();

        let Some(tree) = self.trees.get(&mode) else {
            // No bindings for this mode at all — unbound.
            let drained: Vec<KeyEvent> = self
                .state
                .entry(mode)
                .or_default()
                .buffer
                .drain(..)
                .collect();
            return KeyResolve::Unbound(drained);
        };

        let exact = tree.lookup(&buf);
        let has_longer = tree.has_prefix(&buf);

        match (exact, has_longer) {
            (Some(_binding), true) => {
                // Ambiguous: exact match exists AND deeper bindings exist.
                KeyResolve::Ambiguous
            }
            (Some(binding), false) => {
                // Clean terminal match.
                let binding = binding.clone();
                self.state.entry(mode).or_default().buffer.clear();
                KeyResolve::Match(binding)
            }
            (None, true) => {
                // Prefix only — wait for more keys.
                KeyResolve::Pending
            }
            (None, false) => {
                // Dead end — no match, no prefix.
                let drained: Vec<KeyEvent> = self
                    .state
                    .entry(mode)
                    .or_default()
                    .buffer
                    .drain(..)
                    .collect();
                KeyResolve::Unbound(drained)
            }
        }
    }

    /// Force-resolve any pending chord state (called when the timeout fires).
    ///
    /// If the buffer exactly matches a terminal binding, returns `Match`.
    /// Otherwise returns `Unbound` with the buffered events.
    pub fn timeout_resolve(&mut self, mode: Mode) -> KeyResolve<A> {
        let buf = match self.state.get(&mode) {
            Some(s) if !s.buffer.is_empty() => s.buffer.clone(),
            _ => return KeyResolve::Unbound(vec![]),
        };

        let Some(tree) = self.trees.get(&mode) else {
            let drained: Vec<KeyEvent> = self
                .state
                .entry(mode)
                .or_default()
                .buffer
                .drain(..)
                .collect();
            return KeyResolve::Unbound(drained);
        };

        if let Some(binding) = tree.lookup(&buf) {
            let binding = binding.clone();
            self.state.entry(mode).or_default().buffer.clear();
            KeyResolve::Match(binding)
        } else {
            let drained: Vec<KeyEvent> = self
                .state
                .entry(mode)
                .or_default()
                .buffer
                .drain(..)
                .collect();
            KeyResolve::Unbound(drained)
        }
    }

    /// Return a snapshot of the currently pending chord buffer for `mode`.
    /// Empty when no chord is in progress.
    pub fn pending(&self, mode: Mode) -> &[KeyEvent] {
        self.state
            .get(&mode)
            .map(|s| s.buffer.as_slice())
            .unwrap_or(&[])
    }

    /// Reset the pending buffer for `mode` (e.g. on mode switch).
    pub fn reset(&mut self, mode: Mode) {
        if let Some(state) = self.state.get_mut(&mode) {
            state.buffer.clear();
        }
    }
}
