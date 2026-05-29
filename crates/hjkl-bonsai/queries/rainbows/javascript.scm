; JavaScript rainbow brackets
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
] @rainbow.scope

; Bracket tokens to color.
["(" ")" "[" "]" "{" "}"] @rainbow.bracket
