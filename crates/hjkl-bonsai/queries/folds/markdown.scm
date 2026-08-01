; Markdown folds query — @fold marks foldable multi-line nodes.
; tree-sitter-markdown is a split grammar; these are block-grammar nodes.
;
; Mirrors nvim-treesitter's markdown/folds.scm so `zc` / `zR` fold the same
; regions vim does. `(list_item (list))` is what makes a list item holding a
; sub-list foldable on its own; a bare `(list)` is deliberately NOT captured —
; nvim folds lists through `(section (list))`, so a list in a file with no
; headings does not fold there either.
;
; `(#trim! @fold)` drops the blank rows a `(section)` carries before the next
; heading, so a closed section leaves its separator line visible. The whole
; pattern must be wrapped in parens for the directive to attach to it.

([
  (fenced_code_block)
  (indented_code_block)
  (list_item
    (list))
  (section)
] @fold
  (#trim! @fold))

(section
  (list) @fold
  (#trim! @fold))
