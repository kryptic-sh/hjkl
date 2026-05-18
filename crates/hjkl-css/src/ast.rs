//! Parsed stylesheet representation. Each rule pairs one [`Selector`]
//! (a chain of [`SimpleSelector`]s joined by [`Combinator`]s) with one
//! declaration block.

use crate::value::Value;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
}

/// A compound selector — one or more [`SimpleSelector`]s joined by
/// [`Combinator`]s. `parts.len() == combinators.len() + 1`.
/// `parts[0]` is the leftmost (ancestor/sibling) end; `parts.last()`
/// is the subject that is matched against the target node.
///
/// For a simple flat selector (no combinator) `parts` has one entry and
/// `combinators` is empty.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    pub parts: Vec<SimpleSelector>,
    pub combinators: Vec<Combinator>,
}

/// One simple (non-compound) selector. AND-combined: `button.primary:hover`
/// fills `element=Some("button")`, `classes=["primary"]`, `pseudo=Hover`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SimpleSelector {
    pub element: Option<String>,
    pub classes: Vec<String>,
    pub pseudo: Option<PseudoClass>,
}

/// Relationship between two adjacent [`SimpleSelector`]s in a
/// [`Selector`] chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    /// `.a .b` — `.b` is a descendant (any depth) of `.a`.
    Descendant,
    /// `.a > .b` — `.b` is a direct child of `.a`.
    Child,
    /// `.a + .b` — `.b` immediately follows `.a` as a sibling.
    AdjacentSibling,
    /// `.a ~ .b` — `.b` follows `.a` as any sibling.
    GeneralSibling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PseudoClass {
    Hover,
    Focus,
    Active,
    Disabled,
    Selected,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub property: String,
    pub value: Value,
    /// Set when the source had `!important`. The cascade in
    /// [`crate::Stylesheet::resolve`] honours this — important
    /// declarations beat non-important ones regardless of specificity,
    /// with source order breaking ties within either tier.
    pub important: bool,
}

/// One node in the view tree as far as CSS matching cares.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Node<'a> {
    pub element: &'a str,
    pub classes: &'a [&'a str],
}

impl Selector {
    /// CSS specificity: sum of each part's specificity. Combinators
    /// contribute 0.
    pub fn specificity(&self) -> u32 {
        self.parts.iter().map(SimpleSelector::specificity).sum()
    }

    /// Match against `target` given its `ancestors` (root → parent,
    /// exclusive of the target) and `prev_siblings` (oldest → the
    /// immediately preceding sibling, exclusive of the target).
    /// `state` is the pseudo-class active on the target; ancestors are
    /// always matched without pseudo-class.
    ///
    /// # Sibling combinator limitation (v1)
    /// `AdjacentSibling` and `GeneralSibling` match the sibling against
    /// `prev_siblings`. If the rule continues leftward past the sibling
    /// combinator into a *Descendant* or *Child* step (e.g.
    /// `.grandparent > .prev + .target`), the next combinator is evaluated
    /// against the target's own `ancestors` rather than the sibling's
    /// ancestors. This may false-negative when the continuation requires
    /// introspecting the sibling's subtree context. Fully recursive
    /// sibling-vs-ancestor context requires the adapter to supply
    /// sibling-of-sibling data, which is out of scope for v1. Chained
    /// sibling combinators (`.a + .b + .c`) walk the prev-sibling list
    /// correctly and do not hit this limitation.
    pub fn matches(
        &self,
        target: &Node<'_>,
        ancestors: &[Node<'_>],
        prev_siblings: &[Node<'_>],
        state: Option<PseudoClass>,
    ) -> bool {
        let n = self.parts.len();
        if n == 0 {
            return false;
        }
        // Subject is the rightmost part.
        if !self.parts[n - 1].matches_node(target, state) {
            return false;
        }
        if n == 1 {
            return true;
        }
        // Walk left through the remaining parts. Each Child/Descendant step
        // shrinks `remaining_ancestors`; each AdjacentSibling/GeneralSibling
        // step shrinks `remaining_siblings`.
        let mut remaining_ancestors: &[Node<'_>] = ancestors;
        let mut remaining_siblings: &[Node<'_>] = prev_siblings;
        for i in (0..n - 1).rev() {
            let part = &self.parts[i];
            let combinator = self.combinators[i];
            match combinator {
                Combinator::Descendant => {
                    let pos = remaining_ancestors
                        .iter()
                        .rposition(|a| part.matches_node(a, None));
                    match pos {
                        Some(idx) => {
                            remaining_ancestors = &remaining_ancestors[..idx];
                        }
                        None => return false,
                    }
                }
                Combinator::Child => match remaining_ancestors.last() {
                    Some(parent) if part.matches_node(parent, None) => {
                        remaining_ancestors = &remaining_ancestors[..remaining_ancestors.len() - 1];
                    }
                    _ => return false,
                },
                Combinator::AdjacentSibling => match remaining_siblings.last() {
                    Some(sib) if part.matches_node(sib, None) => {
                        // Consume the matched sibling so a chain of
                        // `+` combinators walks leftward through the
                        // prev-sibling list (`.a + .b + .c`).
                        remaining_siblings = &remaining_siblings[..remaining_siblings.len() - 1];
                    }
                    _ => return false,
                },
                Combinator::GeneralSibling => {
                    let pos = remaining_siblings
                        .iter()
                        .rposition(|s| part.matches_node(s, None));
                    match pos {
                        Some(idx) => {
                            remaining_siblings = &remaining_siblings[..idx];
                        }
                        None => return false,
                    }
                }
            }
        }
        true
    }
}

impl SimpleSelector {
    /// CSS specificity for one simple selector: classes/pseudo each count
    /// 10, type selector counts 1, no IDs in v1.
    pub fn specificity(&self) -> u32 {
        let classes = (self.classes.len() as u32) * 10;
        let pseudo = u32::from(self.pseudo.is_some()) * 10;
        let element = u32::from(self.element.is_some());
        classes + pseudo + element
    }

    /// Does this simple selector match a node in the given state?
    pub fn matches_node(&self, node: &Node<'_>, state: Option<PseudoClass>) -> bool {
        if let Some(want) = &self.element
            && want.as_str() != node.element
        {
            return false;
        }
        if !self
            .classes
            .iter()
            .all(|c| node.classes.contains(&c.as_str()))
        {
            return false;
        }
        match (self.pseudo, state) {
            (None, _) => true,
            (Some(want), Some(have)) => want == have,
            (Some(_), None) => false,
        }
    }
}

impl PseudoClass {
    /// CSS pseudo-class names are ASCII case-insensitive — `:HOVER`,
    /// `:Hover` and `:hover` are all equivalent.
    pub fn from_ident(ident: &str) -> Option<Self> {
        Some(match ident.to_ascii_lowercase().as_str() {
            "hover" => Self::Hover,
            "focus" => Self::Focus,
            "active" => Self::Active,
            "disabled" => Self::Disabled,
            "selected" => Self::Selected,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Hover => "hover",
            Self::Focus => "focus",
            Self::Active => "active",
            Self::Disabled => "disabled",
            Self::Selected => "selected",
        }
    }
}
