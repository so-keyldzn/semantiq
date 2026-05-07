; Elixir symbol extraction queries
; In Elixir, definitions are macro-style `call` nodes. We disambiguate via predicates.

; defmodule Foo do ... end → Module
((call
    target: (identifier) @_target
    (arguments
        (alias) @name))
    (#eq? @_target "defmodule")) @definition.module

; def/defp/defmacro/defmacrop foo(...) → Function (avec parens)
((call
    target: (identifier) @_target
    (arguments
        (call
            target: (identifier) @name)))
    (#match? @_target "^(def|defp|defmacro|defmacrop)$")) @definition.function

; def/defp/defmacro/defmacrop foo (sans parens, identifier direct dans arguments)
((call
    target: (identifier) @_target
    (arguments
        (identifier) @name))
    (#match? @_target "^(def|defp|defmacro|defmacrop)$")) @definition.function
