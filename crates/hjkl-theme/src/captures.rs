use std::collections::HashMap;

use crate::style::StyleSpec;

/// Flat map of tree-sitter capture names to resolved `StyleSpec` values.
///
/// Use `resolve` for fallback-chain lookup; `get` for exact-match only.
#[derive(Clone, Default, Debug)]
pub struct CaptureMap {
    flat: HashMap<String, StyleSpec>,
}

impl CaptureMap {
    pub(crate) fn from_map(flat: HashMap<String, StyleSpec>) -> Self {
        Self { flat }
    }

    /// Exact-match lookup. No fallback.
    pub fn get(&self, capture: &str) -> Option<&StyleSpec> {
        self.flat.get(capture)
    }

    /// Walk the fallback chain: `a.b.c` -> `a.b` -> `a` -> `None`.
    pub fn resolve(&self, capture: &str) -> Option<&StyleSpec> {
        let mut key = capture;
        loop {
            if let Some(spec) = self.flat.get(key) {
                return Some(spec);
            }
            {
                let pos = key.rfind('.')?;
                key = &key[..pos]
            }
        }
    }
}
