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

; Class / struct / interface / enum
(class_declaration
    name: (identifier) @name) @definition.class

(struct_declaration
    name: (identifier) @name) @definition.struct

(interface_declaration
    name: (identifier) @name) @definition.interface

(enum_declaration
    name: (identifier) @name) @definition.enum

; Namespaces (file_scoped or block_scoped)
(namespace_declaration
    name: (identifier) @name) @definition.module

(file_scoped_namespace_declaration
    name: (identifier) @name) @definition.module

; Fields & properties → Variable
(field_declaration
    (variable_declaration
        (variable_declarator
            name: (identifier) @name))) @definition.variable

(property_declaration
    name: (identifier) @name) @definition.variable

; Using directives → Import
(using_directive) @definition.import
