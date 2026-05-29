; Python rainbow brackets
; Scope nodes — each one increments nesting depth.
[
  (argument_list)
  (parameters)
  (list)
  (set)
  (dictionary)
  (tuple)
  (subscript)
  (generator_expression)
  (list_comprehension)
  (set_comprehension)
  (dictionary_comprehension)
  (with_clause)
  (parenthesized_expression)
] @rainbow.scope

; Bracket tokens to color.
["(" ")" "[" "]" "{" "}"] @rainbow.bracket
