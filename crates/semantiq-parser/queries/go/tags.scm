; Go symbol extraction queries
; tree-sitter-go's type_spec uses positional children for name + type.

; Function & method declarations
(function_declaration
    name: (identifier) @name) @definition.function

(method_declaration
    name: (field_identifier) @name) @definition.method

; Type declarations: struct / interface / type alias
; type_spec children are positional: type_identifier then the type body.
(type_spec
    name: (type_identifier) @name
    type: (struct_type)) @definition.struct

(type_spec
    name: (type_identifier) @name
    type: (interface_type)) @definition.interface

; Constants / variables
(const_spec
    name: (identifier) @name) @definition.constant

(var_spec
    name: (identifier) @name) @definition.variable

; Imports — capture whole, name from text
(import_declaration) @definition.import
