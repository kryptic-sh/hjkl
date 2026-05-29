; C++ rainbow brackets
; Scope nodes — each one increments nesting depth.
[
  (compound_statement)
  (argument_list)
  (parameter_list)
  (initializer_list)
  (parenthesized_expression)
  (subscript_expression)
  (enumerator_list)
  (field_declaration_list)
  (template_argument_list)
  (template_parameter_list)
  (lambda_capture_specifier)
  (initializer_list)
  (class_specifier)
  (struct_specifier)
] @rainbow.scope

; Bracket tokens to color.
["(" ")" "[" "]" "{" "}"] @rainbow.bracket
