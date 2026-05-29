; Go rainbow brackets
; Scope nodes — each one increments nesting depth.
[
  (block)
  (argument_list)
  (parameter_list)
  (literal_value)
  (composite_literal)
  (index_expression)
  (slice_expression)
  (parenthesized_type)
  (interface_type)
  (struct_type)
  (type_arguments)
] @rainbow.scope

; Bracket tokens to color.
["(" ")" "[" "]" "{" "}"] @rainbow.bracket
