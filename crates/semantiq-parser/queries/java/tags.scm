; Java symbol extraction queries

; Method declarations
(method_declaration
    name: (identifier) @name) @definition.method

; Constructor (also a method)
(constructor_declaration
    name: (identifier) @name) @definition.method

; Class / interface / enum
(class_declaration
    name: (identifier) @name) @definition.class

(interface_declaration
    name: (identifier) @name) @definition.interface

(enum_declaration
    name: (identifier) @name) @definition.enum

; Field declarations — first declarator's variable name
(field_declaration
    declarator: (variable_declarator
        name: (identifier) @name)) @definition.variable

; Imports — capture whole node, name from text
(import_declaration) @definition.import
