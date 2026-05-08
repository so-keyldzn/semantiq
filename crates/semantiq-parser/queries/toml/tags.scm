; TOML symbol extraction queries (tree-sitter-toml-ng grammar)
;
; Distingue :
; - pair (clé = valeur) → Variable, parent = nom de la table englobante.
; - table ([header]) → Struct (matche le legacy mapping toml_symbol_kind).
; - table_array_element ([[header]]) → Struct.
;
; Les tables avec dotted_key (`[server.deep]`) capturent la dotted_key entière
; comme nom — la dotted notation matche déjà la convention dot-separated.

(pair
    (bare_key) @name) @definition.variable

(pair
    (dotted_key) @name) @definition.variable

(table
    (bare_key) @name) @definition.struct

(table
    (dotted_key) @name) @definition.struct

(table_array_element
    (bare_key) @name) @definition.struct

(table_array_element
    (dotted_key) @name) @definition.struct
