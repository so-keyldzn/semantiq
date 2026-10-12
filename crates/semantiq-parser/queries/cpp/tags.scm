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

; Déclaration de méthode dans la classe (`int add(int);`, `virtual double
; area() const = 0;`) : un field_declaration dont le declarator est une
; fonction. C'est le seul symbole d'une méthode déclarée dans un en-tête et
; définie dans un .cpp, ou purement virtuelle.
(field_declaration_list
    (field_declaration
        declarator: (function_declarator
            declarator: (field_identifier) @name)) @definition.method)

(field_declaration_list
    (declaration
        declarator: (function_declarator
            declarator: (destructor_name) @name)) @definition.method)

; Constructeurs : sans type de retour, leur declarator est un `identifier`
; (pas un `field_identifier`), déclaré (`C();`, `explicit C(int);`) ou défini
; dans la classe (`C() {}`, `C(const C&) = default;`). Déclarés avant le
; pattern « fonction libre » pour gagner la déduplication.
(field_declaration_list
    (declaration
        declarator: (function_declarator
            declarator: (identifier) @name)) @definition.method)

(field_declaration_list
    (function_definition
        declarator: (function_declarator
            declarator: (identifier) @name)) @definition.method)

; Méthode externe via qualified_identifier (Foo::bar ou ns::Foo::bar).
; tree-sitter-cpp imbrique les qualifications ; `extract` garde la feuille
; (`bar`) comme nom et met la portée (`ns::Foo`) dans le parent
; (`QuerySymbolExtractor::split_qualified_name`).
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

; Function-pointer typedef : `typedef int (*callback_t)(int);`
(type_definition
    declarator: (function_declarator
        declarator: (parenthesized_declarator
            (pointer_declarator
                declarator: (type_identifier) @name)))) @definition.type

; Preprocessor (headers are parsed with this grammar) : object-like macros are
; constants, function-like macros functions
(preproc_def
    name: (identifier) @name) @definition.constant

(preproc_function_def
    name: (identifier) @name) @definition.function

; Namespaces
(namespace_definition
    name: (namespace_identifier) @name) @definition.module

; Includes
(preproc_include) @definition.import
