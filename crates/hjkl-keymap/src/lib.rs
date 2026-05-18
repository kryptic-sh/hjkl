//! `hjkl-keymap` — backend-agnostic modal keymap for the hjkl editor stack.
//!
//! Provides:
//! - [`KeyEvent`], [`KeyCode`], [`KeyModifiers`] — backend-agnostic key types.
//! - [`Chord`], [`ChordParseError`] — vim-style chord notation parser/serializer.
//! - [`Keymap`], [`Binding`], [`KeyResolve`], [`Mode`], [`KeymapError`] — stateful dispatch.

pub mod chord;
pub mod key;
pub mod keymap;
pub mod trie;

pub use chord::{Chord, ChordParseError};
pub use key::{KeyCode, KeyEvent, KeyModifiers};
pub use keymap::{KeyResolve, Keymap, KeymapError, Mode};
pub use trie::{Binding, Predicate};
