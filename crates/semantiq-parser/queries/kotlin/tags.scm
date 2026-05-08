; Kotlin symbol extraction queries (tree-sitter-kotlin-ng grammar)
;
; Notes sur le grammar kotlin-ng :
; - Les interfaces et enum classes utilisent class_declaration (pas de nœud
;   dédié interface_declaration / enum_class_declaration). On distingue via
;   le keyword child ("interface") ou le body type (enum_class_body).
; - L'import est un nœud `import` (pas import_header) qui ne peut apparaître
;   qu'après un package_header au tout début du fichier.
; - companion_object, type_alias, enum_entry sont des nœuds distincts.

; Interface : class_declaration avec keyword "interface"
(class_declaration
    "interface"
    (identifier) @name) @definition.interface

; Enum class : class_declaration avec body de type enum_class_body
(class_declaration
    (identifier) @name
    (enum_class_body)) @definition.enum

; Enum entries (les valeurs d'un enum class) → Constant
(enum_entry
    (identifier) @name) @definition.constant

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

; Méthodes dans un companion_object
(companion_object
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

; Companion object (`companion object` ou `companion object Helper`)
(companion_object
    name: (identifier) @name) @definition.class

; Type alias : `typealias Foo = Bar` — le nom est le 1er identifier child
; (le field `type:` pointe sur l'identifier de droite).
(type_alias
    (identifier) @name) @definition.type

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
