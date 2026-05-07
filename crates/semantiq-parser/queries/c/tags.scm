; C symbol extraction queries
; The function name lives in nested declarator chains — we provide patterns
; for the common shapes (plain, pointer, array).

; Function: plain identifier in function_declarator
(function_definition
    declarator: (function_declarator
        declarator: (identifier) @name)) @definition.function

; Function returning pointer: pointer_declarator wraps function_declarator
(function_definition
    declarator: (pointer_declarator
        declarator: (function_declarator
            declarator: (identifier) @name))) @definition.function

; Struct / enum / union with body
(struct_specifier
    name: (type_identifier) @name
    body: (field_declaration_list)) @definition.struct

(union_specifier
    name: (type_identifier) @name
    body: (field_declaration_list)) @definition.struct

(enum_specifier
    name: (type_identifier) @name) @definition.enum

; Type definitions
(type_definition
    declarator: (type_identifier) @name) @definition.type

; Includes
(preproc_include) @definition.import
