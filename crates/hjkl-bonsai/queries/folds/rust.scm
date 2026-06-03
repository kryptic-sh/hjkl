; Rust folds query
; Each @fold capture marks a node whose body should be foldable.
; Vim convention: the node's start_row is the visible "header" line;
; rows start_row+1..=end_row are hidden when the fold is closed.
; Only nodes spanning more than one line produce a meaningful fold.

; Functions, methods, closures
(function_item) @fold
(function_signature_item) @fold

; Impl blocks
(impl_item) @fold

; Type definitions
(struct_item) @fold
(enum_item) @fold
(union_item) @fold
(trait_item) @fold
(type_item) @fold

; Modules
(mod_item) @fold

; Match expressions
(match_expression) @fold

; if/else chains — fold the `if` block and each `else` branch
(if_expression) @fold

; Loop constructs
(loop_expression) @fold
(while_expression) @fold
(for_expression) @fold

; Block expressions used as values
(block) @fold

; Macro definitions
(macro_definition) @fold

; Multi-line macro invocations (e.g. `concat!( … )`, `vec![ … ]`, `lazy_static! { … }`).
; Single-line calls like `println!("x")` are filtered out by the extractor,
; which only folds nodes spanning more than one row.
(macro_invocation) @fold
