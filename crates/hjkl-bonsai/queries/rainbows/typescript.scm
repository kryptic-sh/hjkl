; TypeScript rainbow brackets
; Scope nodes — each one increments nesting depth.
[
  (statement_block)
  (arguments)
  (formal_parameters)
  (array)
  (object)
  (parenthesized_expression)
  (subscript_expression)
  (template_substitution)
  (class_body)
  (switch_body)
  (type_arguments)
  (type_parameters)
  (object_type)
  (tuple_type)
] @rainbow.scope

; Bracket tokens to color.
["(" ")" "[" "]" "{" "}"] @rainbow.bracket
