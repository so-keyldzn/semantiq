; C# symbol extraction queries

; Methods (in class body)
(method_declaration
    name: (identifier) @name) @definition.method

; Local function (function statement, not method)
(local_function_statement
    name: (identifier) @name) @definition.function

; Constructor
(constructor_declaration
    name: (identifier) @name) @definition.method

; Class / struct / record / interface / enum
(class_declaration
    name: (identifier) @name) @definition.class

(struct_declaration
    name: (identifier) @name) @definition.struct

(record_declaration
    name: (identifier) @name) @definition.class

(interface_declaration
    name: (identifier) @name) @definition.interface

(enum_declaration
    name: (identifier) @name) @definition.enum

; Delegate
(delegate_declaration
    name: (identifier) @name) @definition.type

; Namespaces (file_scoped or block_scoped). The `name` field can be an
; identifier OR a qualified_name (e.g. `namespace A.B.C`), so we match `_`.
(namespace_declaration
    name: (_) @name) @definition.module

(file_scoped_namespace_declaration
    name: (_) @name) @definition.module

; Fields & properties → Variable
(field_declaration
    (variable_declaration
        (variable_declarator
            name: (identifier) @name))) @definition.variable

(property_declaration
    name: (identifier) @name) @definition.variable

; Using directives → Import
(using_directive) @definition.import
