; Python symbol extraction queries
; Methods are detected via parent-constrained pattern (function inside class body).

; Methods: function_definition inside class_definition body — must come before
; the generic function_definition pattern so it wins via deduplication.
(class_definition
    body: (block
        (function_definition
            name: (identifier) @name) @definition.method))

; Decorated methods — capture le function_definition interne (pas le
; decorated_definition parent) pour que la dédup par byte range fonctionne
; quand le pattern générique function_definition matche le même nœud.
(class_definition
    body: (block
        (decorated_definition
            (function_definition
                name: (identifier) @name) @definition.method)))

; Top-level / module-level functions
(function_definition
    name: (identifier) @name) @definition.function

; Class definitions
(class_definition
    name: (identifier) @name) @definition.class

; Imports
(import_statement) @definition.import
(import_from_statement) @definition.import
