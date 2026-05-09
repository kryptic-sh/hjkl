//! Trie (prefix tree) data structure backing [`Keymap`].
//!
//! Each node can optionally carry a [`Binding`] (terminal action) and has
//! zero or more children keyed by the next [`KeyEvent`] in the chord.

use std::collections::HashMap;

use crate::key::KeyEvent;

/// The action and metadata stored at a terminal trie node.
#[derive(Debug, Clone)]
pub struct Binding<A> {
    /// The user-defined action associated with this chord.
    pub action: A,
    /// Human-readable description (shown in which-key).
    pub desc: String,
    /// `true` = recursive (`:map`), `false` = non-recursive (`:noremap`).
    /// Ignored in v1 dispatch — reserved for future expansion.
    pub recursive: bool,
}

/// A single node in the trie.
pub(crate) struct TrieNode<A> {
    /// Action bound at this node (if this is a terminal chord).
    pub(crate) action: Option<Binding<A>>,
    /// Children keyed by next key event.
    pub(crate) children: HashMap<KeyEvent, TrieNode<A>>,
}

impl<A> Default for TrieNode<A> {
    fn default() -> Self {
        Self {
            action: None,
            children: HashMap::new(),
        }
    }
}

impl<A: Clone> TrieNode<A> {
    /// Insert a binding at the given chord path (relative to this node).
    pub(crate) fn insert(&mut self, events: &[KeyEvent], binding: Binding<A>) {
        if events.is_empty() {
            self.action = Some(binding);
            return;
        }
        let child = self.children.entry(events[0]).or_default();
        child.insert(&events[1..], binding);
    }

    /// Remove the binding for the given chord path.
    /// Returns `true` if something was removed.
    pub(crate) fn remove(&mut self, events: &[KeyEvent]) -> bool {
        if events.is_empty() {
            let had = self.action.is_some();
            self.action = None;
            return had;
        }
        if let Some(child) = self.children.get_mut(&events[0]) {
            let removed = child.remove(&events[1..]);
            // Prune empty leaf nodes.
            if child.action.is_none() && child.children.is_empty() {
                self.children.remove(&events[0]);
            }
            removed
        } else {
            false
        }
    }

    /// Exact-match lookup: returns the binding if the chord terminates here.
    pub(crate) fn lookup(&self, events: &[KeyEvent]) -> Option<&Binding<A>> {
        if events.is_empty() {
            return self.action.as_ref();
        }
        self.children.get(&events[0])?.lookup(&events[1..])
    }

    /// Returns `true` if `events` is a proper prefix of at least one chord in this trie.
    pub(crate) fn has_prefix(&self, events: &[KeyEvent]) -> bool {
        if events.is_empty() {
            // We are at the node reached by the prefix — it has a prefix if
            // it has any children (deeper chords exist).
            return !self.children.is_empty();
        }
        match self.children.get(&events[0]) {
            Some(child) => child.has_prefix(&events[1..]),
            None => false,
        }
    }

    /// Iterate over direct-child *terminal* bindings reachable from `prefix`.
    /// Each item is `(key_event, &Binding)` for each child node that has an action.
    pub(crate) fn children_of<'a>(
        &'a self,
        prefix: &[KeyEvent],
    ) -> Vec<(&'a KeyEvent, &'a Binding<A>)> {
        if prefix.is_empty() {
            // Return direct children that are terminals.
            return self
                .children
                .iter()
                .filter_map(|(k, node)| node.action.as_ref().map(|b| (k, b)))
                .collect();
        }
        match self.children.get(&prefix[0]) {
            Some(child) => child.children_of(&prefix[1..]),
            None => vec![],
        }
    }
}
