; Ruby symbol extraction queries
; Legacy maps `method` to Function (not Method) — preserve that.

; Method definitions
(method
    name: (identifier) @name) @definition.function

(method
    name: (constant) @name) @definition.function

; Singleton method (def self.foo or def Foo.bar)
(singleton_method
    name: (identifier) @name) @definition.function

(singleton_method
    name: (constant) @name) @definition.function

; Class & module
(class
    name: (constant) @name) @definition.class

(class
    name: (scope_resolution
        name: (constant) @name)) @definition.class

(module
    name: (constant) @name) @definition.module

(module
    name: (scope_resolution
        name: (constant) @name)) @definition.module

; Top-level constant assignment
(assignment
    left: (constant) @name) @definition.constant
