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

; L'attribut est insensible à la casse (`ID="main"`), sa valeur est citée ou
; nue (`id=main`), et la balise peut être auto-fermante (`<input id="q" />`).
((element
    [(start_tag
        (attribute
            (attribute_name) @_attr
            [(quoted_attribute_value (attribute_value) @name)
             (attribute_value) @name]))
     (self_closing_tag
        (attribute
            (attribute_name) @_attr
            [(quoted_attribute_value (attribute_value) @name)
             (attribute_value) @name]))])
    (#match? @_attr "^[iI][dD]$")) @definition.variable

(script_element
    (start_tag
        (tag_name) @name)) @definition.module

(style_element
    (start_tag
        (tag_name) @name)) @definition.module
