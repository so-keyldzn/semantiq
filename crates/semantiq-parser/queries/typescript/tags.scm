; TypeScript symbol extraction queries
; Mirroring SymbolKind precision used by Semantiq legacy extraction.
; Variable-vs-Function disambiguation (arrow / function_expression as const)
; is handled in Rust post-processing, not here.

; Class / interface / enum / type alias
(class_declaration
    name: (type_identifier) @name) @definition.class

(abstract_class_declaration
    name: (type_identifier) @name) @definition.class

(interface_declaration
    name: (type_identifier) @name) @definition.interface

(enum_declaration
    name: (identifier) @name) @definition.enum

(type_alias_declaration
    name: (type_identifier) @name) @definition.type

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

; Import statements — captured whole, name extracted from text like Rust use_declaration
(import_statement) @definition.import
