; ADT definitions — captures spécifiques Semantiq
; Basé sur tree-sitter-rust queries/tags.scm, adapté pour préserver
; la précision des SymbolKind (Struct/Enum/Trait au lieu de Class générique)

(struct_item
    name: (type_identifier) @name) @definition.struct

(enum_item
    name: (type_identifier) @name) @definition.enum

(union_item
    name: (type_identifier) @name) @definition.struct

; type aliases
(type_item
    name: (type_identifier) @name) @definition.type

; method definitions dans impl blocks
(impl_item
    (declaration_list
        (function_item
            name: (identifier) @name) @definition.method))

; method definitions dans trait blocks (default impls)
(trait_item
    (declaration_list
        (function_item
            name: (identifier) @name) @definition.method))

; method signatures dans trait blocks (sans body : `fn bar(&self);`)
; Node distinct dans la grammaire Rust : function_signature_item.
(trait_item
    (declaration_list
        (function_signature_item
            name: (identifier) @name) @definition.method))

; function definitions (top-level et dans les modules)
(source_file
    (function_item
        name: (identifier) @name) @definition.function)

(function_item
    name: (identifier) @name) @definition.function

; trait definitions
(trait_item
    name: (type_identifier) @name) @definition.trait

; module definitions
(mod_item
    name: (identifier) @name) @definition.module

; macro definitions
(macro_definition
    name: (identifier) @name) @definition.macro

; const/static
(const_item
    name: (identifier) @name) @definition.constant

(static_item
    name: (identifier) @name) @definition.constant

; use declarations — on capture le nœud entier, le nom sera extrait du texte
(use_declaration) @definition.import
