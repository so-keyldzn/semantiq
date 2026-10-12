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

; Function-pointer typedef : `typedef int (*callback_t)(int);`
(type_definition
    declarator: (function_declarator
        declarator: (parenthesized_declarator
            (pointer_declarator
                declarator: (type_identifier) @name)))) @definition.type

; Preprocessor : object-like macros are constants, function-like macros functions
(preproc_def
    name: (identifier) @name) @definition.constant

(preproc_function_def
    name: (identifier) @name) @definition.function

; Includes
(preproc_include) @definition.import
