; JSON symbol extraction queries
; Capture le `string_content` pour avoir un nom propre sans guillemets.
; Toutes les clés (top-level et imbriquées) sont capturées ; le parent est
; résolu dot-separated par container_name dans query_extractor.rs.

(pair
    key: (string (string_content) @name)) @definition.variable
