; C++ symbol extraction queries
;
; Notes sur le grammar tree-sitter-cpp :
; - Les méthodes inline (dans `class { ... }`) sont des function_definition
;   directement dans field_declaration_list, dont le declarator est un
;   `function_declarator` avec un `field_identifier` comme nom (pas `identifier`).
; - Les méthodes définies hors classe (`int C::add(int)`) utilisent un
;   `qualified_identifier` imbriqué.
; - Les destructeurs utilisent `destructor_name`, les opérateurs `operator_name`.

; Méthode inline (dans field_declaration_list de class/struct)
(field_declaration_list
    (function_definition
        declarator: (function_declarator
            declarator: (field_identifier) @name)) @definition.method)

; Destructeur inline
(field_declaration_list
    (function_definition
        declarator: (function_declarator
            declarator: (destructor_name) @name)) @definition.method)

; Opérateur inline
(field_declaration_list
    (function_definition
        declarator: (function_declarator
            declarator: (operator_name) @name)) @definition.method)

; Méthode externe via qualified_identifier (Foo::bar ou ns::Foo::bar).
; tree-sitter-cpp imbrique les qualifications, on capture le qualified_identifier
; entier ("ns::Foo::bar") comme nom — le post-traitement de path complet est
; déjà ce qu'attend la résolution parent.
(function_definition
    declarator: (function_declarator
        declarator: (qualified_identifier) @name)) @definition.method

; Function libre (top-level ou namespace) — identifier simple
(function_definition
    declarator: (function_declarator
        declarator: (identifier) @name)) @definition.function

; Function libre retournant un pointeur
(function_definition
    declarator: (pointer_declarator
        declarator: (function_declarator
            declarator: (identifier) @name))) @definition.function

; Class / struct / union — uniquement avec body (pas les forward-declarations)
(class_specifier
    name: (type_identifier) @name
    body: (field_declaration_list)) @definition.class

(struct_specifier
    name: (type_identifier) @name
    body: (field_declaration_list)) @definition.struct

(union_specifier
    name: (type_identifier) @name
    body: (field_declaration_list)) @definition.struct

(enum_specifier
    name: (type_identifier) @name) @definition.enum

; Type definitions
(type_definition
    declarator: (type_identifier) @name) @definition.type

; Namespaces
(namespace_definition
    name: (namespace_identifier) @name) @definition.module

; Includes
(preproc_include) @definition.import
