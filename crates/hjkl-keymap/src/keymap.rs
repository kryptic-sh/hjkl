//! The public [`Keymap`] API that consumers use for chord dispatch.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::chord::{Chord, ChordParseError};
use crate::key::KeyEvent;
use crate::trie::{Binding, Predicate, TrieNode};

/// Trait bound for editor-mode discriminators used as [`Keymap`] map keys.
///
/// Any `Copy + Eq + Hash + Debug` type satisfies this automatically via the
/// blanket impl — consumers define their own concrete enum (e.g. `VimMode`,
/// `HelixMode`) and no manual `impl Mode for T` is needed.
pub trait Mode: Copy + Eq + std::hash::Hash + std::fmt::Debug {}
impl<T: Copy + Eq + std::hash::Hash + std::fmt::Debug> Mode for T {}

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
/// Generic over both the action type `A` and the mode discriminator `M`.
/// `M` can be any `Copy + Eq + Hash + Debug` type — typically a consumer-defined
/// enum such as `VimMode` or `HelixMode`. Chords are stored per-`M` in
/// separate tries. Call [`Keymap::feed`] once per key event; it manages an
/// internal per-mode buffer and returns a [`KeyResolve`] indicating what happened.
pub struct Keymap<A, M: Mode> {
    trees: HashMap<M, TrieNode<A>>,
    leader: char,
    timeout: Duration,
    /// Per-mode chord accumulation state.
    state: HashMap<M, ModeState>,
}

impl<A: Clone, M: Mode> Keymap<A, M> {
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
    /// `action` for `mode` unconditionally.
    pub fn add(
        &mut self,
        mode: M,
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
            condition: None,
        };
        self.add_chord(mode, chord, binding);
        Ok(())
    }

    /// Parse `chord_str` and register `action` for `mode` gated behind a
    /// runtime predicate.
    ///
    /// When the predicate returns `false` at resolve time the binding is
    /// treated as if it does not exist: the key falls through to the next
    /// dispatch layer (engine FSM, tmux fallback, etc.).
    ///
    /// `predicate` must be `Fn() -> bool + Send + Sync + 'static`.  Capture
    /// runtime state via `Arc<Mutex<…>>` or `Arc<AtomicBool>` as needed.
    ///
    /// # When to use this over an always-bound action
    ///
    /// Prefer this when the gate has **no fall-back behaviour** — the
    /// binding should simply "not exist" when the predicate is false, and
    /// the key should reach whatever handler runs after the keymap (engine,
    /// tmux fallback, etc.). Action-variant gating (an always-bound action
    /// that checks state at dispatch time and shows a toast on miss) is
    /// fine when the user benefits from feedback; use `add_if` when silent
    /// fall-through is the desired UX.
    ///
    /// Intentionally retained for future consumers — not yet called by
    /// `apps/hjkl`. Concrete planned callers (issue #120 review):
    ///
    /// - **kryptic-sh/hjkl#39** — scripting (lua / vimscript) layer:
    ///   user-defined conditional bindings (`if filetype == 'rust' then
    ///   bind(...)`) need a host-side predicate primitive.
    /// - **kryptic-sh/hjkl#113** — extension API: third-party plugins
    ///   registering bindings gated on runtime state (debugger attached,
    ///   project type detected, etc.).
    /// - **kryptic-sh/hjkl#115** — git hunk actions: `<leader>hs` / `<leader>hr`
    ///   only meaningful inside a git repo; silent fall-through is cleaner
    ///   than a `not-in-git-repo` toast.
    pub fn add_if(
        &mut self,
        mode: M,
        chord_str: &str,
        action: A,
        desc: &str,
        predicate: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<(), KeymapError> {
        let chord = Chord::parse(chord_str, self.leader)?;
        if chord.is_empty() {
            return Err(KeymapError::EmptyChord);
        }
        let binding = Binding {
            action,
            desc: desc.to_string(),
            recursive: false,
            condition: Some(Arc::new(predicate) as Predicate),
        };
        self.add_chord(mode, chord, binding);
        Ok(())
    }

    /// Register a pre-parsed chord + binding.
    pub fn add_chord(&mut self, mode: M, chord: Chord, binding: Binding<A>) {
        self.trees
            .entry(mode)
            .or_default()
            .insert(&chord.0, binding);
    }

    /// Remove the binding for `chord_str` in `mode`. Returns `Ok(true)` if
    /// something was actually removed.
    pub fn remove(&mut self, mode: M, chord_str: &str) -> Result<bool, KeymapError> {
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
    pub fn children(&self, mode: M, prefix: &Chord) -> Vec<(KeyEvent, Binding<A>)> {
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
    pub fn children_all(&self, mode: M, prefix: &Chord) -> Vec<(KeyEvent, Option<Binding<A>>)> {
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
    pub fn feed(&mut self, mode: M, ev: KeyEvent, _now: Instant) -> KeyResolve<A> {
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
    /// Three outcomes:
    ///
    /// * Buffer matches a terminal binding → `Match(binding)` and the buffer
    ///   is drained. This is the Ambiguous resolution case (e.g. both `g` and
    ///   `gd` bound: pressing `g` and waiting fires the `g` binding).
    /// * Buffer is a pure prefix (no terminal at this depth but deeper
    ///   bindings exist) → `Unbound(vec![])` and the buffer is **left in
    ///   place**. The user is mid-chord; the timeout fired for which-key
    ///   purposes but no chord-level action is required.
    /// * Buffer is a dead-end (no terminal, no descendants) → `Unbound(buf)`
    ///   with the drained events. This shouldn't normally occur given that
    ///   `feed` only buffers keys that extend a valid prefix.
    pub fn timeout_resolve(&mut self, mode: M) -> KeyResolve<A> {
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
        } else if tree.has_prefix(&buf) {
            // Pure-Pending: user is mid-chord. Keep the buffer alive.
            KeyResolve::Unbound(vec![])
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
    pub fn pending(&self, mode: M) -> &[KeyEvent] {
        self.state
            .get(&mode)
            .map(|s| s.buffer.as_slice())
            .unwrap_or(&[])
    }

    /// Reset the pending buffer for `mode` (e.g. on mode switch).
    pub fn reset(&mut self, mode: M) {
        if let Some(state) = self.state.get_mut(&mode) {
            state.buffer.clear();
        }
    }

    /// Pop the last key from the pending buffer for `mode`.
    /// Returns the removed key, or `None` if the buffer was empty.
    ///
    /// Used by callers (e.g. which-key popup) to implement Backspace-as-navigate:
    /// the user backs out of a chord prefix one key at a time.
    pub fn pop(&mut self, mode: M) -> Option<KeyEvent> {
        self.state.get_mut(&mode)?.buffer.pop()
    }
}
