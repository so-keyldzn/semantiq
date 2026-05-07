; PHP symbol extraction queries

; Function definition (top-level)
(function_definition
    name: (name) @name) @definition.function

; Method declaration (inside class/trait body)
(method_declaration
    name: (name) @name) @definition.method

; Class / interface / trait / enum
(class_declaration
    name: (name) @name) @definition.class

(interface_declaration
    name: (name) @name) @definition.interface

(trait_declaration
    name: (name) @name) @definition.trait

(enum_declaration
    name: (name) @name) @definition.enum

; Namespace (Module)
(namespace_definition
    name: (namespace_name) @name) @definition.module

; Constants
(const_declaration
    (const_element
        (name) @name)) @definition.constant

; Use declarations → Import
(namespace_use_declaration) @definition.import
