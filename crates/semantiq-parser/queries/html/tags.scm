; HTML symbol extraction queries
;
; Sémantique : seuls les éléments **top-level** (enfants directs de `document`)
; sont capturés. Sans ce parent constraint, un fichier HTML moyen avec quelques
; centaines de divs imbriqués ferait exploser l'index FTS5 et les embeddings
; pour aucun gain de recherche utile.

(document
    (element
        (start_tag
            (tag_name) @name)) @definition.variable)

(document
    (script_element
        (start_tag
            (tag_name) @name)) @definition.module)

(document
    (style_element
        (start_tag
            (tag_name) @name)) @definition.module)
