#[cfg(test)]
mod bug_probes {
    use semantiq_parser::language::{Language, LanguageSupport};
    use semantiq_parser::symbols::{Symbol, SymbolExtractor, SymbolKind};

    fn extract(lang: Language, source: &str) -> Vec<Symbol> {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(lang, source).unwrap();
        SymbolExtractor::extract(&tree, source, lang).unwrap()
    }

    // B1: Python decorated method must NOT be duplicated.
    #[test]
    fn b1_python_decorated_method() {
        let source = r#"
class A:
    @staticmethod
    def m(): pass
"#;
        let symbols = extract(Language::Python, source);
        let m_symbols: Vec<_> = symbols.iter().filter(|s| s.name == "m").collect();
        println!("B1 - Python decorated method 'm':");
        for sym in &m_symbols {
            println!("    kind={:?}, parent={:?}", sym.kind, sym.parent);
        }
        assert_eq!(m_symbols.len(), 1, "expected exactly 1 symbol for decorated method");
        assert_eq!(m_symbols[0].kind, SymbolKind::Method);
        assert_eq!(m_symbols[0].parent.as_deref(), Some("A"));
    }

    // B2: Elixir def must inherit parent from enclosing defmodule.
    #[test]
    fn b2_elixir_parent_lost() {
        let source = r#"
defmodule M do
  def foo, do: :ok
end
"#;
        let symbols = extract(Language::Elixir, source);
        let foo = symbols.iter().find(|s| s.name == "foo").expect("foo missing");
        println!("B2 - Elixir parent for 'foo': parent={:?}, kind={:?}", foo.parent, foo.kind);
        assert_eq!(foo.kind, SymbolKind::Function);
        assert_eq!(foo.parent.as_deref(), Some("M"));
    }

    // B3: defmacro / defmacrop must be captured (not just def/defp).
    #[test]
    fn b3_elixir_defmacro() {
        let source = r#"
defmodule M do
  defmacro mac(x), do: x
  defmacrop privmac(x), do: x
end
"#;
        let symbols = extract(Language::Elixir, source);
        assert!(
            symbols.iter().any(|s| s.name == "mac" && s.kind == SymbolKind::Function),
            "defmacro 'mac' must be captured, got: {:?}",
            symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        assert!(
            symbols.iter().any(|s| s.name == "privmac" && s.kind == SymbolKind::Function),
            "defmacrop 'privmac' must be captured"
        );
    }

    // B4: nested defmodule produces dot-separated parent path.
    #[test]
    fn b4_nested_defmodule() {
        let source = r#"
defmodule A do
  defmodule B do
    def foo, do: :ok
  end
end
"#;
        let symbols = extract(Language::Elixir, source);
        let foo = symbols.iter().find(|s| s.name == "foo").expect("foo missing");
        println!("B4 - Nested Elixir parent for 'foo': {:?}", foo.parent);
        assert_eq!(foo.parent.as_deref(), Some("A.B"));
    }

    // B5: Kotlin interface/enum extracted as Class
    #[test]
    fn b5_kotlin_kinds() {
        let source = r#"
interface Greeter { fun greet(): String }
enum class Status { ACTIVE, INACTIVE }
"#;
        let symbols = extract(Language::Kotlin, source);
        println!("B5 - Kotlin kinds:");
        for sym in &symbols {
            println!("  name={}, kind={:?}", sym.name, sym.kind);
        }
    }

    // B7: Kotlin parent resolution
    #[test]
    fn b7_kotlin_parent() {
        let source = r#"
class Foo {
    fun bar(): Int = 1
    val nested: String = "x"
}
"#;
        let symbols = extract(Language::Kotlin, source);
        println!("B7 - Kotlin parent resolution:");
        for sym in symbols.iter().filter(|s| matches!(s.name.as_str(), "bar" | "nested")) {
            println!("  name={}, parent={:?}", sym.name, sym.parent);
        }
    }

    // B8: C++ inline method must be extracted with class as parent.
    #[test]
    fn b8_cpp_inline() {
        let source = r#"
class C {
public:
    int add(int n) { return n + 1; }
    ~C() {}
    int operator+(int n) { return n; }
};
"#;
        let symbols = extract(Language::Cpp, source);
        println!("B8 - C++ inline methods:");
        for s in symbols.iter().filter(|s| matches!(s.kind, SymbolKind::Method)) {
            println!("  name={}, parent={:?}", s.name, s.parent);
        }
        let add = symbols.iter().find(|s| s.name == "add").expect("inline `add` missing");
        assert_eq!(add.kind, SymbolKind::Method);
        assert_eq!(add.parent.as_deref(), Some("C"));
    }

    // B9: HTML must capture only top-level elements (children of `document`),
    // not every nested div / p / span — otherwise the index explodes on real HTML.
    #[test]
    fn b9_html_recursion() {
        let source = r#"<html><body><div><p>hello</p><p>world</p></div></body></html>"#;
        let symbols = extract(Language::Html, source);
        println!("B9 - HTML elements extracted:");
        for sym in &symbols {
            println!("  name={}, kind={:?}", sym.name, sym.kind);
        }
        // Seul `html` doit être extrait (enfant direct de document).
        assert_eq!(symbols.len(), 1, "HTML must extract only top-level elements");
        assert_eq!(symbols[0].name, "html");
    }

    // B10: JSON nested keys must have dot-separated parent path.
    #[test]
    fn b10_json_nesting() {
        let source = r#"{"a": {"b": {"c": 1}}, "d": [1, 2]}"#;
        let symbols = extract(Language::Json, source);
        println!("B10 - JSON nested keys:");
        for sym in &symbols {
            println!("  name={}, parent={:?}", sym.name, sym.parent);
        }
        let by_name: std::collections::HashMap<&str, &Symbol> =
            symbols.iter().map(|s| (s.name.as_str(), s)).collect();
        assert_eq!(by_name["a"].parent, None);
        assert_eq!(by_name["b"].parent.as_deref(), Some("a"));
        assert_eq!(by_name["c"].parent.as_deref(), Some("a.b"));
        assert_eq!(by_name["d"].parent, None);
    }

    // Imports must capture only the short name, not the full statement text.
    // Tests M2/M4/N5 from the review (Rust use, Python import, PHP use).
    #[test]
    fn imports_name_field() {
        let rust_imp = extract(Language::Rust, "use std::collections::HashMap;");
        let rust_name = &rust_imp.iter().find(|s| s.kind == SymbolKind::Import).unwrap().name;
        assert_eq!(rust_name, "HashMap", "Rust import name must be short");

        let php_imp = extract(Language::Php, "<?php use Foo\\Bar;");
        let php_name = &php_imp.iter().find(|s| s.kind == SymbolKind::Import).unwrap().name;
        assert_eq!(php_name, "Bar", "PHP import name must be short");

        let py_imp = extract(Language::Python, "import os");
        let py_name = &py_imp.iter().find(|s| s.kind == SymbolKind::Import).unwrap().name;
        assert_eq!(py_name, "os", "Python simple import name must be short");

        // No leading "use " / trailing ";" / spaces in the name field.
        for src in [
            "use std::collections::HashMap;",
            "<?php use Foo\\Bar;",
            "import os",
        ] {
            let lang = match src.starts_with("use ") {
                true => Language::Rust,
                false if src.starts_with("<?php") => Language::Php,
                _ => Language::Python,
            };
            for s in extract(lang, src).iter().filter(|s| s.kind == SymbolKind::Import) {
                assert!(!s.name.contains(';'), "import name has trailing ';': {:?}", s.name);
                assert!(!s.name.contains(' '), "import name has spaces: {:?}", s.name);
                assert!(!s.name.starts_with("use"), "import name leaks keyword: {:?}", s.name);
            }
        }
    }
}
