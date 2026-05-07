; Bash symbol extraction queries

; Function definitions: foo() { } or function foo { }
(function_definition
    name: (word) @name) @definition.function

; Variable assignments: FOO=bar
(variable_assignment
    name: (variable_name) @name) @definition.variable
