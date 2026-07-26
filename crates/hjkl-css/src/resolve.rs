//! Stylesheet → declaration list for a given (target, ancestors, state).
//!
//! Walks every rule in source order, keeps the best per-property match
//! ranked by `(important, specificity, rule_idx, decl_idx)`. `!important`
//! wins over any non-important declaration regardless of specificity;
//! within either group, higher specificity wins, with rule order (then
//! intra-rule declaration order) breaking ties.

use std::collections::HashMap;

use crate::ast::{Node, PseudoClass, Rule, Stylesheet};
use crate::value::Value;

/// Resolved style for one node — property -> value, with cascade already
/// applied. Adapter crates (e.g. hjkl-css-floem) convert this to their
/// own builder type.
///
/// Internally each property stores the `(rule_idx, decl_idx)` of the
/// winning declaration so that [`iter`](ResolvedStyle::iter) can replay
/// properties in CSS source order, which is the correct semantic for
/// adapters that apply shorthand/longhand overrides.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedStyle {
    /// Maps property name → (winning `(rule_idx, decl_idx)`, value).
    /// `decl_idx` is the declaration's position inside its rule, capturing
    /// intra-rule source order for shorthand/longhand replay.
    pub(crate) properties: HashMap<String, ((usize, usize), Value)>,
}

impl ResolvedStyle {
    pub fn get(&self, property: &str) -> Option<&Value> {
        self.properties.get(property).map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.properties.len()
    }

    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    /// Iterate declarations in **CSS source order**: ascending `rule_idx`
    /// of the winning declaration, with `decl_idx` (position inside the
    /// rule) breaking ties. The result matches the order in which the
    /// declarations appeared in the stylesheet so that adapters applying
    /// properties sequentially produce CSS-spec behaviour for
    /// shorthand/longhand collisions.
    ///
    /// Example: `x { border-color: blue; border: 1px solid red; }` yields
    /// `("border-color", blue)` then `("border", red)`. An adapter that
    /// applies `border-color` first then `border` ends up with red, which
    /// is the CSS-correct winner.
    ///
    /// If you need alphabetical order for snapshots or serialization,
    /// collect the iterator and sort the resulting `Vec` by key.
    ///
    /// **Performance:** allocates a `Vec` and sorts it on every call. For
    /// one-shot adapter conversion this cost is negligible.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        type Entry<'a> = (&'a String, &'a ((usize, usize), Value));
        let mut entries: Vec<Entry<'_>> = self.properties.iter().collect();
        // Primary: ascending (rule_idx, decl_idx) — CSS source order.
        // Secondary: alphabetical key. The secondary tie-break is
        // unreachable today because `decl_idx` is the enumerate index of
        // a declaration inside its rule, so two declarations cannot share
        // the same (rule_idx, decl_idx). It is kept anyway to lock in a
        // deterministic order in case the cascade ever stores synthetic
        // entries that bypass the enumerate invariant.
        entries.sort_by(|(ka, (pa, _)), (kb, (pb, _))| pa.cmp(pb).then_with(|| ka.cmp(kb)));
        entries.into_iter().map(|(k, (_, v))| (k.as_str(), v))
    }
}

/// Per-property cascade key. Ordering: higher tuple wins.
/// `(important, specificity, rule_idx, decl_idx)`.
type CascadeKey = (bool, u32, usize, usize);

impl Stylesheet {
    /// Resolve every property that targets `target` given its `ancestors`
    /// (root → parent, exclusive of `target`) and `prev_siblings` (oldest
    /// → immediately preceding sibling, exclusive of `target`). `state` is
    /// the pseudo-class active on `target`.
    pub fn resolve(
        &self,
        target: &Node<'_>,
        ancestors: &[Node<'_>],
        prev_siblings: &[Node<'_>],
        state: Option<PseudoClass>,
    ) -> ResolvedStyle {
        let mut best: HashMap<String, (CascadeKey, Value)> = HashMap::new();
        for (rule_idx, rule) in self.rules.iter().enumerate() {
            let Some(spec) =
                best_matching_specificity(rule, target, ancestors, prev_siblings, state)
            else {
                continue;
            };
            for (decl_idx, decl) in rule.declarations.iter().enumerate() {
                let key: CascadeKey = (decl.important, spec, rule_idx, decl_idx);
                let replace = match best.get(&decl.property) {
                    Some((existing, _)) => existing <= &key,
                    None => true,
                };
                if replace {
                    best.insert(decl.property.clone(), (key, decl.value.clone()));
                }
            }
        }
        ResolvedStyle {
            // Store (rule_idx, decl_idx, value) so iter() can replay in
            // full CSS source order, including intra-rule order.
            properties: best
                .into_iter()
                .map(|(k, ((_, _, rule_idx, decl_idx), v))| (k, ((rule_idx, decl_idx), v)))
                .collect(),
        }
    }
}

fn best_matching_specificity(
    rule: &Rule,
    target: &Node<'_>,
    ancestors: &[Node<'_>],
    prev_siblings: &[Node<'_>],
    state: Option<PseudoClass>,
) -> Option<u32> {
    rule.selectors
        .iter()
        .filter(|s| s.matches(target, ancestors, prev_siblings, state))
        .map(|s| s.specificity())
        .max()
}
