; Markdown folds query — @fold marks foldable multi-line nodes.
; tree-sitter-markdown is a split grammar; these are block-grammar nodes.

(section) @fold
(fenced_code_block) @fold
(list) @fold
(block_quote) @fold
