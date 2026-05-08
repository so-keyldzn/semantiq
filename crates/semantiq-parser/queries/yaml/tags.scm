; YAML symbol extraction queries
; Keys-as-symbols semantics matching legacy.

(block_mapping_pair
    key: (flow_node) @name) @definition.variable

; Flow mappings (`{key: val}`, syntaxe inline JSON-like légale en YAML)
(flow_pair
    key: (flow_node) @name) @definition.variable
