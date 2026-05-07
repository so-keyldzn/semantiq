; Scala symbol extraction queries

; Function definitions
(function_definition
    name: (identifier) @name) @definition.function

; Class / object / trait / enum
(class_definition
    name: (identifier) @name) @definition.class

(object_definition
    name: (identifier) @name) @definition.class

(trait_definition
    name: (identifier) @name) @definition.trait

(enum_definition
    name: (identifier) @name) @definition.enum

; Type definitions
(type_definition
    name: (type_identifier) @name) @definition.type

; val / var definitions → Variable
(val_definition
    pattern: (identifier) @name) @definition.variable

(var_definition
    pattern: (identifier) @name) @definition.variable

; Imports
(import_declaration) @definition.import
