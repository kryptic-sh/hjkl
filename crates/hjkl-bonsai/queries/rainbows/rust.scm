; Rust rainbow brackets
; Scope nodes — each one increments nesting depth.
[
  (block)
  (arguments)
  (parameters)
  (array_expression)
  (tuple_expression)
  (tuple_type)
  (struct_expression)
  (use_list)
  (type_arguments)
  (type_parameters)
  (closure_parameters)
  (token_tree)
  (declaration_list)
  (field_declaration_list)
  (enum_variant_list)
  (match_block)
  (where_clause)
] @rainbow.scope

; Bracket tokens to color.
["(" ")" "[" "]" "{" "}"] @rainbow.bracket
