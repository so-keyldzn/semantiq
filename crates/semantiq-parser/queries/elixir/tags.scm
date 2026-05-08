; Elixir symbol extraction queries
; In Elixir, definitions are macro-style `call` nodes. We disambiguate via predicates.

; defmodule / defprotocol Foo do ... end → Module
((call
    target: (identifier) @_target
    (arguments
        (alias) @name))
    (#match? @_target "^(defmodule|defprotocol)$")) @definition.module

; def/defp/defmacro/defmacrop/defguard/defguardp/defdelegate/defn/defnp foo(...) → Function (avec parens)
((call
    target: (identifier) @_target
    (arguments
        (call
            target: (identifier) @name)))
    (#match? @_target "^(def|defp|defmacro|defmacrop|defguard|defguardp|defdelegate|defn|defnp)$")) @definition.function

; Idem mais avec une guard clause `when` — la signature est wrappée dans un binary_operator
((call
    target: (identifier) @_target
    (arguments
        (binary_operator
            left: (call
                target: (identifier) @name)
            operator: "when")))
    (#match? @_target "^(def|defp|defmacro|defmacrop|defguard|defguardp|defn|defnp)$")) @definition.function

; def/defp/defmacro/defmacrop foo (sans parens, identifier direct dans arguments)
((call
    target: (identifier) @_target
    (arguments
        (identifier) @name))
    (#match? @_target "^(def|defp|defmacro|defmacrop|defguard|defguardp|defdelegate|defn|defnp)$")) @definition.function
