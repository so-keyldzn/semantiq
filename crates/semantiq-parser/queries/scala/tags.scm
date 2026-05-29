; Scala symbol extraction queries

; Package clause → Module
(package_clause
    name: (_) @name) @definition.module

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

; Enum cases (Scala 3) — `case Red, Green, Blue` ou `case Some(x: Int)`.
(simple_enum_case
    name: (identifier) @name) @definition.constant

(full_enum_case
    name: (identifier) @name) @definition.class

; Type definitions
(type_definition
    name: (type_identifier) @name) @definition.type

; val / var definitions (avec body) → Variable
(val_definition
    pattern: (identifier) @name) @definition.variable

(var_definition
    pattern: (identifier) @name) @definition.variable

; Multi-binding sur une seule ligne : `val a, b = 1` / `var x, y = 0`.
; Le grammar regroupe les noms dans un nœud `identifiers`. On capture chaque
; identifier : le dédup côté Rust inclut la plage du nom, donc les deux symboles
; survivent (sinon seul le premier serait indexé).
(val_definition
    pattern: (identifiers (identifier) @name)) @definition.variable

(var_definition
    pattern: (identifiers (identifier) @name)) @definition.variable

; val / var declarations (sans body, membres abstraits dans un trait) → Variable
(val_declaration
    name: (identifier) @name) @definition.variable

(var_declaration
    name: (identifier) @name) @definition.variable

; Imports
(import_declaration) @definition.import
