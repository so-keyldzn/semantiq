; HTML symbol extraction queries
;
; Sémantique : les éléments **top-level** (enfants directs de `document`,
; `<html>`), les éléments porteurs d'un `id` à n'importe quelle profondeur
; (nommés par la valeur de l'id : `<div id="main">` → `main`) et les
; `<script>` / `<style>`. Les autres éléments imbriqués ne sont pas capturés :
; quelques centaines de divs anonymes feraient exploser l'index FTS5 et les
; embeddings pour aucun gain de recherche.

(document
    (element
        (start_tag
            (tag_name) @name)) @definition.variable)

((element
    (start_tag
        (attribute
            (attribute_name) @_attr
            (quoted_attribute_value (attribute_value) @name))))
    (#eq? @_attr "id")) @definition.variable

(script_element
    (start_tag
        (tag_name) @name)) @definition.module

(style_element
    (start_tag
        (tag_name) @name)) @definition.module
