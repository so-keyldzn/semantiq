; Kotlin symbol extraction queries (tree-sitter-kotlin-ng grammar)
;
; Notes sur le grammar kotlin-ng :
; - Les interfaces et enum classes utilisent class_declaration (pas de nœud
;   dédié interface_declaration / enum_class_declaration). On distingue via
;   le keyword child ("interface") ou le body type (enum_class_body).
; - L'import est un nœud `import` (pas import_header) qui ne peut apparaître
;   qu'après un package_header au tout début du fichier.

; Interface : class_declaration avec keyword "interface"
(class_declaration
    "interface"
    (identifier) @name) @definition.interface

; Enum class : class_declaration avec body de type enum_class_body
(class_declaration
    (identifier) @name
    (enum_class_body)) @definition.enum

; Method : function_declaration imbriquée dans class_body
; Doit précéder le pattern function_declaration générique pour gagner via dédup.
(class_declaration
    (class_body
        (function_declaration
            (identifier) @name) @definition.method))

(object_declaration
    (class_body
        (function_declaration
            (identifier) @name) @definition.method))

; Class normale : class_declaration avec keyword "class"
; Matche aussi enum class (qui a "class" en keyword) — l'ordre des patterns
; ci-dessus garantit qu'enum est sélectionné via dédup (start_byte identique).
(class_declaration
    "class"
    (identifier) @name) @definition.class

; Object
(object_declaration
    (identifier) @name) @definition.class

; Top-level function
(function_declaration
    (identifier) @name) @definition.function

; Property (variable de classe ou top-level)
(property_declaration
    (variable_declaration
        (identifier) @name)) @definition.variable

; Import (présent uniquement si placé après package_header)
(import
    (qualified_identifier) @name) @definition.import
