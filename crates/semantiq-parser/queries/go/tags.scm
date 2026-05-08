; Go symbol extraction queries
; tree-sitter-go's type_spec uses positional children for name + type.

; Function & method declarations
(function_declaration
    name: (identifier) @name) @definition.function

(method_declaration
    name: (field_identifier) @name) @definition.method

; Type declarations: struct / interface / type alias / defined types
; type_spec children are positional: type_identifier then the type body.
;
; Patterns plus spécifiques (struct/interface) en premier — gagne via dédup
; sur le pattern générique (type_spec) qui suit.
(type_spec
    name: (type_identifier) @name
    type: (struct_type)) @definition.struct

(type_spec
    name: (type_identifier) @name
    type: (interface_type)) @definition.interface

; Type aliases & defined types (ni struct ni interface) :
; `type MyInt int`, `type Handler = http.Handler`, `type FooFn func(int) error`.
(type_spec
    name: (type_identifier) @name) @definition.type

; Constants / variables
(const_spec
    name: (identifier) @name) @definition.constant

(var_spec
    name: (identifier) @name) @definition.variable

; Imports — capture whole, name from text
(import_declaration) @definition.import
