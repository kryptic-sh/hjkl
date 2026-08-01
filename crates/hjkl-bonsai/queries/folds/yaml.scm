; YAML folds query — @fold marks foldable multi-line nodes.
;
; Anchored on the PAIR / ITEM, not on the `(block_mapping)` container: a
; container starts at its first child, so `key:` was never the fold's header
; line, and the document-level mapping produced one fold starting at the first
; key that swallowed every sibling below it. Matches nvim-treesitter's
; yaml/folds.scm.

[
  (block_mapping_pair)
  (block_sequence_item)
] @fold
