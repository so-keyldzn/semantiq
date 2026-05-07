; JavaScript symbol extraction queries
; JS grammar lacks interface/enum/type_alias compared to TypeScript.

; Class declarations
(class_declaration
    name: (identifier) @name) @definition.class

; Method definitions (inside class body)
(method_definition
    name: (property_identifier) @name) @definition.method

; Function declarations
(function_declaration
    name: (identifier) @name) @definition.function

(generator_function_declaration
    name: (identifier) @name) @definition.function

; Variable bindings — kind may be upgraded to Function in post-processing
; if value is arrow_function or function_expression
(lexical_declaration
    (variable_declarator
        name: (identifier) @name)) @definition.variable

(variable_declaration
    (variable_declarator
        name: (identifier) @name)) @definition.variable

; Import statements
(import_statement) @definition.import
