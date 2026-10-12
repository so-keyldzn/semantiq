#[cfg(test)]
mod bug_probes {
    use semantiq_parser::language::{Language, LanguageSupport};
    use semantiq_parser::references::{Reference, ReferenceExtractor, ReferenceKind};
    use semantiq_parser::structure::StructureExtractor;
    use semantiq_parser::symbols::{Symbol, SymbolExtractor, SymbolKind};
    use semantiq_parser::{ImportExtractor, ImportKind};
    use std::path::Path;

    fn extract(lang: Language, source: &str) -> Vec<Symbol> {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(lang, source).unwrap();
        SymbolExtractor::extract(&tree, source, lang).unwrap()
    }

    fn references(lang: Language, source: &str) -> Vec<Reference> {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(lang, source).unwrap();
        ReferenceExtractor::extract(&tree, source, lang)
    }

    /// `(line, kind)` of every reference named `name`.
    fn kinds_of(refs: &[Reference], name: &str) -> Vec<(usize, ReferenceKind)> {
        refs.iter()
            .filter(|r| r.name == name)
            .map(|r| (r.line, r.kind))
            .collect()
    }

    /// `(type_name, super_name, kind)` type relations of `source`.
    fn relations(lang: Language, source: &str) -> Vec<(String, String, &'static str)> {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(lang, source).unwrap();
        StructureExtractor::type_relations(&tree, source, lang)
            .into_iter()
            .map(|r| (r.type_name, r.super_name, r.kind.as_str()))
            .collect()
    }

    /// `(caller, callee, line)` call edges of `source`.
    fn call_edges(lang: Language, source: &str) -> Vec<(String, String, usize)> {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(lang, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, lang).unwrap();
        let refs = ReferenceExtractor::extract(&tree, source, lang);
        StructureExtractor::extract(&tree, source, lang, &symbols, &refs)
            .calls
            .into_iter()
            .map(|e| (e.caller, e.callee, e.line))
            .collect()
    }

    /// Everything the indexer extracts from one file, parsed with the grammar
    /// its path selects (`.tsx` → TSX).
    struct Extracted {
        has_error: bool,
        symbols: Vec<Symbol>,
        refs: Vec<Reference>,
        calls: Vec<(String, String, usize)>,
        imports: Vec<(String, Option<String>, ImportKind)>,
    }

    fn extract_file(path: &str, source: &str) -> Extracted {
        let path = Path::new(path);
        let lang = Language::from_path(path).unwrap();
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse_path(lang, path, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, lang).unwrap();
        let refs = ReferenceExtractor::extract(&tree, source, lang);
        let calls = StructureExtractor::extract(&tree, source, lang, &symbols, &refs)
            .calls
            .into_iter()
            .map(|e| (e.caller, e.callee, e.line))
            .collect();
        let imports = ImportExtractor::extract(&tree, source, lang)
            .unwrap()
            .into_iter()
            .map(|i| (i.path, i.name, i.kind))
            .collect();
        Extracted {
            has_error: tree.root_node().has_error(),
            symbols,
            refs,
            calls,
            imports,
        }
    }

    fn reference<'a>(refs: &'a [Reference], name: &str, line: usize) -> Option<&'a Reference> {
        refs.iter().find(|r| r.name == name && r.line == line)
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
        assert_eq!(
            m_symbols.len(),
            1,
            "expected exactly 1 symbol for decorated method"
        );
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
        let foo = symbols
            .iter()
            .find(|s| s.name == "foo")
            .expect("foo missing");
        println!(
            "B2 - Elixir parent for 'foo': parent={:?}, kind={:?}",
            foo.parent, foo.kind
        );
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
            symbols
                .iter()
                .any(|s| s.name == "mac" && s.kind == SymbolKind::Function),
            "defmacro 'mac' must be captured, got: {:?}",
            symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "privmac" && s.kind == SymbolKind::Function),
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
        let foo = symbols
            .iter()
            .find(|s| s.name == "foo")
            .expect("foo missing");
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
        let kind = |name: &str| {
            symbols
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} missing"))
                .kind
        };
        assert_eq!(kind("Greeter"), SymbolKind::Interface);
        assert_eq!(kind("greet"), SymbolKind::Method);
        assert_eq!(kind("Status"), SymbolKind::Enum);
        assert_eq!(kind("ACTIVE"), SymbolKind::Constant);
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
        for name in ["bar", "nested"] {
            let sym = symbols
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(sym.parent.as_deref(), Some("Foo"), "{name}");
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
        for s in symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
        {
            println!("  name={}, parent={:?}", s.name, s.parent);
        }
        let add = symbols
            .iter()
            .find(|s| s.name == "add")
            .expect("inline `add` missing");
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
        assert_eq!(
            symbols.len(),
            1,
            "HTML must extract only top-level elements"
        );
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
        let rust_name = &rust_imp
            .iter()
            .find(|s| s.kind == SymbolKind::Import)
            .unwrap()
            .name;
        assert_eq!(rust_name, "HashMap", "Rust import name must be short");

        let php_imp = extract(Language::Php, "<?php use Foo\\Bar;");
        let php_name = &php_imp
            .iter()
            .find(|s| s.kind == SymbolKind::Import)
            .unwrap()
            .name;
        assert_eq!(php_name, "Bar", "PHP import name must be short");

        let py_imp = extract(Language::Python, "import os");
        let py_name = &py_imp
            .iter()
            .find(|s| s.kind == SymbolKind::Import)
            .unwrap()
            .name;
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
            for s in extract(lang, src)
                .iter()
                .filter(|s| s.kind == SymbolKind::Import)
            {
                assert!(
                    !s.name.contains(';'),
                    "import name has trailing ';': {:?}",
                    s.name
                );
                assert!(
                    !s.name.contains(' '),
                    "import name has spaces: {:?}",
                    s.name
                );
                assert!(
                    !s.name.starts_with("use"),
                    "import name leaks keyword: {:?}",
                    s.name
                );
            }
        }
    }

    // B11 (#59): Elixir `def` / `defp` heads are definitions, not calls. The
    // keyword itself is not indexed and no call edge points at `def`/`defp` or
    // at the function being defined.
    #[test]
    fn b11_elixir_def_head_is_definition() {
        let source =
            "defmodule M do\n  def foo(x) do\n    bar(x)\n  end\n  defp bar(x), do: x\nend\n";
        let refs = references(Language::Elixir, source);
        assert_eq!(kinds_of(&refs, "foo"), vec![(2, ReferenceKind::Definition)]);
        assert_eq!(
            kinds_of(&refs, "bar"),
            vec![(3, ReferenceKind::Call), (5, ReferenceKind::Definition)]
        );
        assert!(kinds_of(&refs, "def").is_empty());
        assert!(kinds_of(&refs, "defp").is_empty());
        assert!(kinds_of(&refs, "defmodule").is_empty());

        let edges = call_edges(Language::Elixir, source);
        assert_eq!(edges, vec![("foo".to_string(), "bar".to_string(), 3)]);
    }

    // B12 (#60): Rust calls inside macro arguments (`assert_eq!`, `println!`,
    // `vec!`, `tracing::info!`) are `call` references with call edges.
    #[test]
    fn b12_rust_calls_inside_macros() {
        let source = "fn compute(x: u32) -> u32 { x }\nfn test_it() {\n    assert_eq!(compute(1), 1);\n    println!(\"{}\", compute(2));\n    let v = vec![compute(3)];\n    tracing::info!(n = compute(4), \"done\");\n}\n";
        let refs = references(Language::Rust, source);
        assert_eq!(
            kinds_of(&refs, "compute"),
            vec![
                (1, ReferenceKind::Definition),
                (3, ReferenceKind::Call),
                (4, ReferenceKind::Call),
                (5, ReferenceKind::Call),
                (6, ReferenceKind::Call),
            ]
        );
        let edges = call_edges(Language::Rust, source);
        let compute_edges: Vec<usize> = edges
            .iter()
            .filter(|(caller, callee, _)| caller == "test_it" && callee == "compute")
            .map(|(_, _, line)| *line)
            .collect();
        assert_eq!(compute_edges, vec![3, 4, 5, 6]);
    }

    // B13 (#64): a function named in an attribute string (`#[serde(default =
    // "f")]`, `deserialize_with`, `with`) gets a `reference` occurrence, so
    // `dead-code` no longer flags serde helpers. Ordinary strings still never
    // match.
    #[test]
    fn b13_attribute_strings_are_references() {
        let source = "fn default_true() -> bool { true }\nfn parse_level<'de, D>(d: D) -> Result<u8, D::Error> { todo!() }\n#[derive(serde::Deserialize)]\npub struct Cfg {\n    #[serde(default = \"default_true\")]\n    pub on: bool,\n    #[serde(deserialize_with = \"parse_level\")]\n    pub level: u8,\n}\nfn main() { let s = \"default_true\"; }\n";
        let refs = references(Language::Rust, source);
        assert_eq!(
            kinds_of(&refs, "default_true"),
            vec![
                (1, ReferenceKind::Definition),
                (5, ReferenceKind::Reference)
            ]
        );
        assert_eq!(
            kinds_of(&refs, "parse_level"),
            vec![
                (2, ReferenceKind::Definition),
                (7, ReferenceKind::Reference)
            ]
        );
    }

    // B14 (#61): the doc comment of an exported TS/JS symbol sits above the
    // `export` wrapper, the comment of a decorated Python function above the
    // decorator, and a Rust doc comment above `#[derive]`: all are attached.
    #[test]
    fn b14_doc_comment_through_wrappers() {
        let doc = |lang: Language, src: &str, name: &str| -> Option<String> {
            extract(lang, src)
                .into_iter()
                .find(|s| s.name == name)
                .and_then(|s| s.doc_comment)
        };
        assert_eq!(
            doc(
                Language::TypeScript,
                "/** Greets. */\nexport function greet(n: string) { return n; }\n",
                "greet"
            )
            .as_deref(),
            Some("/** Greets. */")
        );
        assert_eq!(
            doc(
                Language::TypeScript,
                "/** Doc */\nexport default class Foo {}\n",
                "Foo"
            )
            .as_deref(),
            Some("/** Doc */")
        );
        assert_eq!(
            doc(
                Language::JavaScript,
                "/** Greets. */\nexport function greet(n) { return n; }\n",
                "greet"
            )
            .as_deref(),
            Some("/** Greets. */")
        );
        assert_eq!(
            doc(
                Language::Python,
                "# Handles the route\n@app.route('/')\ndef index():\n    pass\n",
                "index"
            )
            .as_deref(),
            Some("# Handles the route")
        );
        assert_eq!(
            doc(
                Language::Rust,
                "/// Config.\n#[derive(Debug)]\npub struct Cfg {}\n",
                "Cfg"
            )
            .as_deref(),
            Some("/// Config.")
        );
    }

    // B15 (#54): `.tsx` files are parsed with the TSX grammar. With the plain
    // TypeScript grammar the JSX yielded ERROR nodes, `format(user)` inside the
    // markup was a plain reference and tag / attribute names were `type`s.
    #[test]
    fn b15_tsx_jsx_parsed_with_tsx_grammar() {
        let source = include_str!("fixtures/typescript/sample.tsx");
        let e = extract_file("src/App.tsx", source);
        assert!(!e.has_error, "TSX fixture must parse without ERROR nodes");

        // Exported components are definitions (their `export` visibility in
        // the signature is B19), and the declarations after the JSX survive.
        let app = e.symbols.iter().find(|s| s.name == "App").expect("App");
        assert_eq!(app.kind, SymbolKind::Function);
        assert_eq!(
            reference(&e.refs, "App", 19).map(|r| r.kind),
            Some(ReferenceKind::Definition)
        );
        for name in ["Panel", "Unused"] {
            assert!(
                e.symbols
                    .iter()
                    .any(|s| s.name == name && s.kind == SymbolKind::Function),
                "{name} missing after the JSX block: {:?}",
                e.symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
            );
        }

        // A rendered component is a call: `<Button />` → edge App -> Button,
        // `<Ui.Card>` → App -> Card, `<App />` → Panel -> App.
        assert_eq!(
            reference(&e.refs, "Button", 24).map(|r| r.kind),
            Some(ReferenceKind::Call)
        );
        for edge in [
            ("App", "Button", 24),
            ("App", "Card", 25),
            ("App", "format", 25),
            ("App", "setCount", 28),
            ("Panel", "App", 34),
        ] {
            let (caller, callee, line) = edge;
            assert!(
                e.calls
                    .iter()
                    .any(|(c, k, l)| c == caller && k == callee && *l == line),
                "missing edge {caller} -> {callee} L{line}: {:?}",
                e.calls
            );
        }

        // Attribute values are references, a call inside the markup is a call.
        assert_eq!(
            reference(&e.refs, "handle", 24).map(|r| r.kind),
            Some(ReferenceKind::Reference)
        );
        assert_eq!(
            reference(&e.refs, "format", 25).map(|r| r.kind),
            Some(ReferenceKind::Call)
        );
        assert_eq!(
            reference(&e.refs, "count", 26).map(|r| r.kind),
            Some(ReferenceKind::Reference)
        );

        // Attribute names, intrinsic tags and closing tags are not symbols.
        for name in [
            "className",
            "onClick",
            "label",
            "title",
            "div",
            "button",
            "p",
        ] {
            assert!(
                !e.refs.iter().any(|r| r.name == name),
                "`{name}` must not be a reference: {:?}",
                e.refs
            );
        }
        assert!(reference(&e.refs, "Card", 27).is_none(), "closing tag");
        assert!(reference(&e.refs, "Ui", 27).is_none(), "closing tag");

        // The JavaScript grammar parses JSX too (`.jsx`, `.js`): the same
        // rules apply, plus namespaced names and lowercase member tags.
        let jsx = extract_file(
            "src/App.jsx",
            "export function App() {\n  return (\n    <svg:rect xlink:href=\"#a\" onClick={go}>\n      <Button label=\"x\" />\n      <Ui.Card />\n      <motion.div />\n    </svg:rect>\n  );\n}\n",
        );
        assert!(!jsx.has_error);
        assert_eq!(
            reference(&jsx.refs, "Button", 4).map(|r| r.kind),
            Some(ReferenceKind::Call)
        );
        assert_eq!(
            reference(&jsx.refs, "Card", 5).map(|r| r.kind),
            Some(ReferenceKind::Call)
        );
        assert_eq!(
            reference(&jsx.refs, "go", 3).map(|r| r.kind),
            Some(ReferenceKind::Reference)
        );
        assert!(
            jsx.calls
                .iter()
                .any(|(c, k, l)| c == "App" && k == "Button" && *l == 4),
            "{:?}",
            jsx.calls
        );
        for name in ["label", "onClick", "svg", "rect", "xlink", "href", "div"] {
            assert!(
                !jsx.refs.iter().any(|r| r.name == name),
                "`{name}` must not be a reference: {:?}",
                jsx.refs
            );
        }
        assert_eq!(
            reference(&jsx.refs, "motion", 6).map(|r| r.kind),
            Some(ReferenceKind::Reference)
        );
    }

    // B16 (#56): Kotlin imports come from `import` nodes (kotlin-ng grammar).
    #[test]
    fn b16_kotlin_imports_extracted() {
        let source = include_str!("fixtures/kotlin/sample.kt");
        let e = extract_file("src/Sample.kt", source);
        assert_eq!(
            e.imports,
            vec![(
                "kotlin.io.println".to_string(),
                Some("println".to_string()),
                ImportKind::Std
            )]
        );
        let e = extract_file(
            "src/X.kt",
            "package a.b\nimport foo.bar.Baz\nimport kotlin.collections.List as KList\nimport foo.*\n\nclass X {\n    fun f() { Baz() }\n}\n",
        );
        assert_eq!(
            e.imports.len(),
            3,
            "one Import per import line: {:?}",
            e.imports
        );
        assert_eq!(
            e.imports[1].1.as_deref(),
            Some("KList"),
            "alias is the name"
        );
        assert_eq!(e.imports[2].1, None, "wildcard has no binding name");
    }

    // B17 (#57): `import X as Y` and `import a, b`.
    #[test]
    fn b17_python_aliased_and_multi_imports() {
        let e = extract_file(
            "pkg/mod.py",
            "import numpy as np\nimport os, sys\nfrom . import sibling\n",
        );
        assert_eq!(
            e.imports,
            vec![
                (
                    "numpy".to_string(),
                    Some("np".to_string()),
                    ImportKind::External
                ),
                ("os".to_string(), Some("os".to_string()), ImportKind::Std),
                ("sys".to_string(), Some("sys".to_string()), ImportKind::Std),
                (
                    ".".to_string(),
                    Some("sibling".to_string()),
                    ImportKind::Local
                ),
            ]
        );
    }

    // B18 (#58): barrel files. Re-exports and `require()` are imports; a
    // re-exported name is not a definition (it is an import when it has a
    // source, a plain reference for `export { qux }`).
    #[test]
    fn b18_ts_barrel_reexports() {
        let e = extract_file(
            "src/index.ts",
            "export { greet, Foo as Bar } from './foo';\nexport * from './bar';\nexport { qux };\nconst { helper } = require('./util');\n",
        );
        assert_eq!(
            e.imports
                .iter()
                .map(|(p, _, k)| (p.as_str(), *k))
                .collect::<Vec<_>>(),
            vec![
                ("./foo", ImportKind::Local),
                ("./bar", ImportKind::Local),
                ("./util", ImportKind::Local),
            ]
        );
        assert!(
            !e.symbols.iter().any(|s| s.kind != SymbolKind::Import
                && matches!(s.name.as_str(), "greet" | "Foo" | "Bar" | "qux")),
            "re-exported names are not symbol definitions: {:?}",
            e.symbols
        );
        for (name, kind) in [
            ("greet", ReferenceKind::Import),
            ("Foo", ReferenceKind::Import),
            ("Bar", ReferenceKind::Import),
        ] {
            assert_eq!(
                reference(&e.refs, name, 1).map(|r| r.kind),
                Some(kind),
                "{name}"
            );
        }
        assert_eq!(
            reference(&e.refs, "qux", 3).map(|r| r.kind),
            Some(ReferenceKind::Reference)
        );
        assert!(
            !e.refs.iter().any(|r| r.kind == ReferenceKind::Definition
                && matches!(r.name.as_str(), "greet" | "Foo" | "Bar" | "qux")),
            "{:?}",
            e.refs
        );
    }

    // B19 (#63): `export` is part of the signature of TS/JS declarations, as
    // `pub` is for Rust, so `dead-code` sees exported symbols as public.
    #[test]
    fn b19_ts_export_in_signature() {
        let e = extract_file(
            "src/a.ts",
            "export default function Panel() { return 1; }\nexport function Unused() { return null; }\nexport const unusedConst = () => 1;\nexport class K {}\nexport interface I {}\nfunction local() {}\nconst v = 1;\n",
        );
        let sig = |name: &str| {
            e.symbols
                .iter()
                .find(|s| s.name == name)
                .and_then(|s| s.signature.clone())
                .unwrap_or_default()
        };
        assert_eq!(
            sig("Panel"),
            "export default function Panel() { return 1; }"
        );
        assert_eq!(sig("Unused"), "export function Unused() { return null; }");
        assert_eq!(sig("unusedConst"), "export const unusedConst = () => 1;");
        assert_eq!(sig("K"), "export class K {}");
        assert_eq!(sig("I"), "export interface I {}");
        assert_eq!(sig("local"), "function local() {}");
        assert_eq!(sig("v"), "const v = 1;");

        // Decorators are children of the `export_statement`, not of the class:
        // they stay out of the signature (NestJS `@Injectable()`, a multi-line
        // Angular `@Component({...})` would otherwise push `export` past the
        // first `(` and the class name past the 200-char truncation).
        let decorated = extract_file(
            "src/c.ts",
            "@Injectable()\nexport class Service {}\n@Component({\n  selector: 'x',\n})\nexport default class Cmp {}\n",
        );
        let sig = |name: &str| {
            decorated
                .symbols
                .iter()
                .find(|s| s.name == name)
                .and_then(|s| s.signature.clone())
                .unwrap_or_default()
        };
        assert_eq!(sig("Service"), "export class Service {}");
        assert_eq!(sig("Cmp"), "export default class Cmp {}");

        let js = extract_file(
            "src/b.js",
            "export function f() {}\nexport default class C {}\n",
        );
        let sig = |name: &str| {
            js.symbols
                .iter()
                .find(|s| s.name == name)
                .and_then(|s| s.signature.clone())
                .unwrap_or_default()
        };
        assert_eq!(sig("f"), "export function f() {}");
        assert_eq!(sig("C"), "export default class C {}");
    }

    // B20 (#66): comments inside a Rust `use` group are not path segments.
    #[test]
    fn b20_rust_use_group_comments() {
        let e = extract_file(
            "src/lib.rs",
            "use foo::{ // keep\n    Bar, /* c */ Baz,\n};\n",
        );
        assert_eq!(
            e.imports
                .iter()
                .map(|(p, _, _)| p.as_str())
                .collect::<Vec<_>>(),
            vec!["foo::Bar", "foo::Baz"]
        );
    }

    // B21 (#66): Scala selector imports and PHP group imports yield one
    // import per leaf, like the Rust brace expansion.
    #[test]
    fn b21_scala_php_brace_imports() {
        let e = extract_file("src/A.scala", "import a.b.{C, D => E}\n");
        assert_eq!(
            e.imports,
            vec![
                (
                    "a.b.C".to_string(),
                    Some("C".to_string()),
                    ImportKind::External
                ),
                (
                    "a.b.D".to_string(),
                    Some("E".to_string()),
                    ImportKind::External
                ),
            ]
        );
        let e = extract_file("src/a.php", "<?php\nuse Foo\\{A, B as C};\n");
        assert_eq!(
            e.imports,
            vec![
                (
                    "Foo\\A".to_string(),
                    Some("A".to_string()),
                    ImportKind::External
                ),
                (
                    "Foo\\B".to_string(),
                    Some("C".to_string()),
                    ImportKind::External
                ),
            ]
        );
    }

    // B22 (#66): call edges on a file with many symbols and calls attach each
    // call to its own function (the line-sorted sweep replaces the calls x
    // symbols scan; `test_call_edges_sweep_matches_naive_scan` in
    // `structure.rs` is the guard of its exactness).
    #[test]
    fn b22_call_edges_on_a_large_file() {
        let mut source = String::new();
        for i in 0..4000 {
            source.push_str(&format!(
                "fn f{i}() {{\n    aa();\n    bb();\n    cc();\n}}\n"
            ));
        }
        let e = extract_file("src/big.rs", &source);
        assert_eq!(e.calls.len(), 12_000);
        assert!(
            e.calls
                .iter()
                .enumerate()
                .all(|(i, (caller, _, _))| *caller == format!("f{}", i / 3)),
            "each call attached to its own function"
        );
    }

    // B30 (#141): a PHP variable `$helper` is not a reference to `helper`;
    // `$this` produces nothing; `$this->helper` and `helper()` still count.
    #[test]
    fn b30_php_variables_are_not_references() {
        let r = references(
            Language::Php,
            "<?php\nfunction helper(float $x): float {\n    $helper = 5;\n    return $this->helper + helper();\n}\n",
        );
        assert_eq!(
            kinds_of(&r, "helper"),
            vec![(2, ReferenceKind::Definition), (4, ReferenceKind::Call)]
        );
        assert!(kinds_of(&r, "this").is_empty());
        assert!(r.iter().all(|x| x.line != 3), "{r:?}");
    }

    // B31 (#82, #149): enum members, TS interface signatures and TS enum
    // members are declared names, not usages.
    #[test]
    fn b31_declaration_site_names_are_definitions() {
        let rust = references(
            Language::Rust,
            "enum Kind { Alpha, Beta(u8) }\nstruct Alpha;\n",
        );
        assert_eq!(
            kinds_of(&rust, "Alpha"),
            vec![
                (1, ReferenceKind::Definition),
                (2, ReferenceKind::Definition)
            ]
        );
        assert_eq!(
            kinds_of(&rust, "Beta"),
            vec![(1, ReferenceKind::Definition)]
        );

        let java = references(Language::Java, "enum E { ALPHA, BETA(1) }\n");
        assert_eq!(
            kinds_of(&java, "ALPHA"),
            vec![(1, ReferenceKind::Definition)]
        );
        assert_eq!(
            kinds_of(&java, "BETA"),
            vec![(1, ReferenceKind::Definition)]
        );

        let kotlin = references(Language::Kotlin, "enum class Color { RED, GREEN }\n");
        assert_eq!(
            kinds_of(&kotlin, "RED"),
            vec![(1, ReferenceKind::Definition)]
        );
        assert_eq!(
            kinds_of(&kotlin, "GREEN"),
            vec![(1, ReferenceKind::Definition)]
        );

        let php = references(
            Language::Php,
            "<?php\nenum Color: string { case Red = 'r'; case Green = 'g'; }\n",
        );
        assert_eq!(kinds_of(&php, "Red"), vec![(2, ReferenceKind::Definition)]);
        assert_eq!(
            kinds_of(&php, "Green"),
            vec![(2, ReferenceKind::Definition)]
        );

        let ts = references(
            Language::TypeScript,
            "interface Runner {\n  run(): void;\n  name: string;\n}\nabstract class C { abstract go(): void }\nenum E { Alpha, Beta = 2, Gamma, Delta }\n",
        );
        assert_eq!(kinds_of(&ts, "run"), vec![(2, ReferenceKind::Definition)]);
        assert_eq!(kinds_of(&ts, "name"), vec![(3, ReferenceKind::Definition)]);
        assert_eq!(kinds_of(&ts, "go"), vec![(5, ReferenceKind::Definition)]);
        assert_eq!(kinds_of(&ts, "Alpha"), vec![(6, ReferenceKind::Definition)]);
        assert_eq!(kinds_of(&ts, "Beta"), vec![(6, ReferenceKind::Definition)]);
        // Every unassigned member, not only the first: `enum_body` repeats
        // its `name` field and `child_by_field_id` sees only the first one.
        assert_eq!(kinds_of(&ts, "Gamma"), vec![(6, ReferenceKind::Definition)]);
        assert_eq!(kinds_of(&ts, "Delta"), vec![(6, ReferenceKind::Definition)]);
    }

    // B32 (#149): PHP `implements` names are type references, like
    // `extends`.
    #[test]
    fn b32_php_implements_are_type_references() {
        let r = references(
            Language::Php,
            "<?php\nfinal class Circle extends Base implements Shape, \\JsonSerializable {}\n",
        );
        assert_eq!(kinds_of(&r, "Base"), vec![(2, ReferenceKind::Type)]);
        assert_eq!(kinds_of(&r, "Shape"), vec![(2, ReferenceKind::Type)]);
        assert_eq!(
            kinds_of(&r, "JsonSerializable"),
            vec![(2, ReferenceKind::Type)]
        );
    }

    // B33 (#80): calls with type arguments are calls, with a call edge.
    #[test]
    fn b33_generic_calls_are_calls() {
        let rust = references(
            Language::Rust,
            "fn make_it(x: u32) -> u32 { x }\nfn t() {\n    let w = make_it::<u32>(1);\n    let n = s.parse::<u32>();\n    let z = std::mem::size_of::<u64>();\n    let f = make_it::<u32>;\n}\n",
        );
        assert_eq!(
            kinds_of(&rust, "make_it"),
            vec![
                (1, ReferenceKind::Definition),
                (3, ReferenceKind::Call),
                (6, ReferenceKind::Reference)
            ]
        );
        assert_eq!(kinds_of(&rust, "parse"), vec![(4, ReferenceKind::Call)]);
        assert_eq!(kinds_of(&rust, "size_of"), vec![(5, ReferenceKind::Call)]);
        assert_eq!(kinds_of(&rust, "mem"), vec![(5, ReferenceKind::Reference)]);
        assert_eq!(
            call_edges(
                Language::Rust,
                "fn make_it(x: u32) -> u32 { x }\nfn t() {\n    make_it::<u32>(1);\n}\n"
            ),
            vec![("t".to_string(), "make_it".to_string(), 3)]
        );

        let cpp = references(
            Language::Cpp,
            "void t() {\n    auto p = std::make_unique<int>(1);\n    auto q = make_unique<int>(1);\n    obj.get<int>(2);\n}\n",
        );
        assert_eq!(
            kinds_of(&cpp, "make_unique"),
            vec![(2, ReferenceKind::Call), (3, ReferenceKind::Call)]
        );
        assert_eq!(kinds_of(&cpp, "get"), vec![(4, ReferenceKind::Call)]);
        assert_eq!(kinds_of(&cpp, "obj"), vec![(4, ReferenceKind::Reference)]);
    }

    // B34 (#152): Ruby `include` / `extend` / `prepend` in a class or module
    // body are `implements` relations; inside a method they are not.
    #[test]
    fn b34_ruby_mixins_are_relations() {
        let r = relations(
            Language::Ruby,
            "class Dog < Animal\n  include Walkable, M::Other\n  extend Enumerable\n  prepend Loud\n  extend self\n  def x\n    include Nope\n  end\nend\nmodule Helpers\n  include Foo\nend\n",
        );
        assert_eq!(
            r,
            vec![
                ("Dog", "Animal", "extends"),
                ("Dog", "Enumerable", "implements"),
                ("Dog", "Loud", "implements"),
                ("Dog", "Other", "implements"),
                ("Dog", "Walkable", "implements"),
                ("Helpers", "Foo", "implements"),
            ]
            .into_iter()
            .map(|(t, s, k)| (t.to_string(), s.to_string(), k))
            .collect::<Vec<_>>()
        );
    }

    // B35 (#153): a Kotlin interface with modifiers extends its supertypes
    // (it does not implement them).
    #[test]
    fn b35_kotlin_interface_with_modifiers_extends() {
        let r = relations(
            Language::Kotlin,
            "interface Shape : Base\npublic interface A : B\nsealed interface S : B\n@Ann interface C : B\nclass K : B\n",
        );
        let kind = |t: &str| {
            r.iter()
                .find(|(type_name, _, _)| type_name == t)
                .map(|(_, _, k)| *k)
                .unwrap_or_else(|| panic!("{t} missing"))
        };
        assert_eq!(kind("Shape"), "extends");
        assert_eq!(kind("A"), "extends");
        assert_eq!(kind("S"), "extends");
        assert_eq!(kind("C"), "extends");
        assert_eq!(kind("K"), "implements");
    }

    /// #78: the import and chunk extractors walked the tree with one native
    /// frame per level and overflowed the 2 MiB stack of the `spawn_blocking`
    /// threads of `serve` on a few thousand nested nodes (a `SIGABRT`, not a
    /// panic, so the indexer died again at every restart). Both walks are
    /// iterative now; this runs them on a 2 MiB stack over 10 000 nested JSON
    /// arrays and a 10 000-term JavaScript expression.
    #[test]
    fn b23_deep_ast_does_not_overflow_the_stack() {
        use semantiq_parser::ChunkExtractor;

        let json = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
        let js = format!("var x = {};\n", vec!["1"; 10_000].join(" + "));
        let sources = vec![(Language::Json, json), (Language::JavaScript, js)];

        let worker = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                let mut support = LanguageSupport::new().unwrap();
                for (lang, source) in &sources {
                    let tree = support.parse(*lang, source).unwrap();
                    let imports = ImportExtractor::extract(&tree, source, *lang).unwrap();
                    assert!(imports.is_empty(), "{lang:?}: {imports:?}");
                    let chunks = ChunkExtractor::new().extract(&tree, source, *lang).unwrap();
                    assert!(!chunks.is_empty(), "{lang:?}");
                }
            })
            .unwrap();
        worker.join().expect("deep AST extraction must not crash");
    }

    /// #84: a Ruby `call` whose text merely starts with `require`
    /// (`requirements.push("json")`) is no import, a receiver (`Kernel.require`)
    /// is not a `require` directive; an Elixir brace alias `alias A.{B, C}`
    /// gives one import per leaf instead of the fragment `A.{B`.
    #[test]
    fn b24_ruby_and_elixir_text_imports() {
        let ruby = extract_file(
            "lib/app.rb",
            "require 'json'\nrequire_relative 'helpers'\nrequirements.push(\"json\")\n\
             require_all('x')\nrequire './local'\n",
        );
        assert_eq!(
            ruby.imports,
            vec![
                (
                    "json".to_string(),
                    Some("json".to_string()),
                    ImportKind::External
                ),
                (
                    "./helpers".to_string(),
                    Some("helpers".to_string()),
                    ImportKind::Local
                ),
                (
                    "./local".to_string(),
                    Some("local".to_string()),
                    ImportKind::Local
                ),
            ]
        );

        let elixir = extract_file(
            "lib/app.ex",
            "defmodule App do\n  alias App.Models.{User, Order}\n  alias App.Repo, as: R\n  \
             import Enum, only: [map: 2]\n  use GenServer\n  require Logger\n  \
             alias Deep.{A.{B, C}, D}\nend\n",
        );
        let paths: Vec<&str> = elixir.imports.iter().map(|(p, _, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "App.Models.User",
                "App.Models.Order",
                "App.Repo",
                "Enum",
                "GenServer",
                "Logger",
                "Deep.A.B",
                "Deep.A.C",
                "Deep.D",
            ]
        );
        assert_eq!(
            elixir.imports[2].1.as_deref(),
            Some("R"),
            "`as:` alias is the name"
        );
        assert_eq!(elixir.imports[0].1.as_deref(), Some("User"));
    }

    /// #143: `mod x;` is a `Local` import of the module (its file path with
    /// `#[path]`), an inline module is none; `super::` / `self::` paths
    /// written inside inline modules are rebased on the file's module so the
    /// resolver, which only knows the file, finds the right target.
    #[test]
    fn b25_rust_mod_declarations_and_inline_super() {
        let e = extract_file(
            "src/lib.rs",
            "mod sub;\npub mod other;\n#[cfg(test)]\n#[path = \"shapes/circle.rs\"]\nmod circle;\n\
             mod inline { pub fn f() {} }\nuse self::other::Thing;\n\
             #[cfg(test)]\nmod tests {\n    use super::*;\n    use super::other::Thing;\n    \
             use super::super::up::X;\n    use self::fixtures::F;\n    use crate::sub::S;\n    \
             mod nested { use super::super::other::Deep; }\n}\n",
        );
        assert_eq!(
            e.imports,
            vec![
                (
                    "sub".to_string(),
                    Some("sub".to_string()),
                    ImportKind::Local
                ),
                (
                    "other".to_string(),
                    Some("other".to_string()),
                    ImportKind::Local
                ),
                (
                    "shapes/circle.rs".to_string(),
                    Some("circle".to_string()),
                    ImportKind::Local
                ),
                (
                    "self::other::Thing".to_string(),
                    Some("Thing".to_string()),
                    ImportKind::Local
                ),
                ("self".to_string(), None, ImportKind::Local),
                (
                    "self::other::Thing".to_string(),
                    Some("Thing".to_string()),
                    ImportKind::Local
                ),
                (
                    "super::up::X".to_string(),
                    Some("X".to_string()),
                    ImportKind::Local
                ),
                (
                    "self::tests::fixtures::F".to_string(),
                    Some("F".to_string()),
                    ImportKind::Local
                ),
                (
                    "crate::sub::S".to_string(),
                    Some("S".to_string()),
                    ImportKind::Local
                ),
                (
                    "self::other::Deep".to_string(),
                    Some("Deep".to_string()),
                    ImportKind::Local
                ),
            ]
        );
    }

    /// #147: imports of a package the project declares are `local` for Java,
    /// Kotlin, Scala, C#, PHP and Go without `go.mod`; the C# alias form
    /// `using X = A.B;` imports `A.B`, not `X`.
    #[test]
    fn b26_project_packages_are_local() {
        use semantiq_parser::ImportContext;
        let mut context = ImportContext::default();
        context.add_source_file(
            "src/main/java/com/acme/models/User.java",
            Language::Java,
            "package com.acme.models;\n",
        );
        context.add_source_file(
            "src/main/scala/com/acme/svc/Svc.scala",
            Language::Scala,
            "package com.acme\npackage svc\n",
        );
        context.add_source_file(
            "Models/User.cs",
            Language::CSharp,
            "namespace App.Models;\n",
        );
        context.add_source_file(
            "src/Models/User.php",
            Language::Php,
            "<?php\n\nnamespace App\\Models;\n",
        );
        context.add_source_file("internal/store/store.go", Language::Go, "");
        let kinds = |path: &str, source: &str| {
            let lang = Language::from_path(Path::new(path)).unwrap();
            let mut support = LanguageSupport::new().unwrap();
            let tree = support.parse(lang, source).unwrap();
            ImportExtractor::extract_with_context(&tree, source, lang, &context)
                .unwrap()
                .into_iter()
                .map(|i| (i.path, i.kind))
                .collect::<Vec<_>>()
        };
        let local = |p: &str| (p.to_string(), ImportKind::Local);
        let external = |p: &str| (p.to_string(), ImportKind::External);
        let std = |p: &str| (p.to_string(), ImportKind::Std);

        assert_eq!(
            kinds(
                "src/main/java/com/acme/api/A.java",
                "import com.acme.models.User;\nimport com.acme.models.*;\n\
                 import static com.acme.models.User.NAME;\nimport com.acme.other.X;\n\
                 import java.util.List;\nimport com.google.common.base.Strings;\n"
            ),
            vec![
                local("com.acme.models.User"),
                local("com.acme.models"),
                local("com.acme.models.User.NAME"),
                external("com.acme.other.X"),
                std("java.util.List"),
                external("com.google.common.base.Strings"),
            ]
        );
        assert_eq!(
            kinds(
                "src/main/kotlin/com/acme/K.kt",
                "import com.acme.models.User\nimport com.acme.svc.Svc\nimport kotlinx.coroutines.flow.Flow\n"
            ),
            vec![
                local("com.acme.models.User"),
                local("com.acme.svc.Svc"),
                std("kotlinx.coroutines.flow.Flow"),
            ]
        );
        assert_eq!(
            kinds(
                "src/main/scala/com/acme/B.scala",
                "import com.acme.svc.{Svc, Other}\nimport com.acme.Thing\nimport cats.effect.IO\n"
            ),
            vec![
                local("com.acme.svc.Svc"),
                local("com.acme.svc.Other"),
                local("com.acme.Thing"),
                external("cats.effect.IO"),
            ]
        );
        assert_eq!(
            kinds(
                "Services/Svc.cs",
                "using System;\nusing System.Linq;\nusing App.Models;\nusing static App.Models.User;\n\
                 using U = App.Models.User;\nusing Newtonsoft.Json;\nusing SystemX.Y;\n"
            ),
            vec![
                std("System"),
                std("System.Linq"),
                local("App.Models"),
                local("App.Models.User"),
                local("App.Models.User"),
                external("Newtonsoft.Json"),
                external("SystemX.Y"),
            ]
        );
        assert_eq!(
            kinds(
                "src/Services/Svc.php",
                "<?php\nnamespace App\\Services;\nuse App\\Models\\User;\nuse App\\Models\\{Order, Item};\n\
                 use Illuminate\\Support\\Str;\n"
            ),
            vec![
                local("App\\Models\\User"),
                local("App\\Models\\Order"),
                local("App\\Models\\Item"),
                external("Illuminate\\Support\\Str"),
            ]
        );
        assert_eq!(
            kinds(
                "cmd/app/main.go",
                "package main\nimport (\n\t\"fmt\"\n\t\"net/http\"\n\t\"myapp/internal/store\"\n\
                 \t\"github.com/x/y\"\n)\n"
            ),
            vec![
                std("fmt"),
                std("net/http"),
                local("myapp/internal/store"),
                external("github.com/x/y"),
            ]
        );
    }

    /// Review of #172: a `mod child;` written inside an inline module loads
    /// `outer/child.rs`, so it is stored `self::outer::child` (`outer/x.rs`
    /// for a `#[path]` attribute); `require_relative 'x'` is stored `./x` so
    /// the resolver tells it from `require 'x'`; an import of the module path
    /// of `go.mod` itself is the local root package; a C# namespace and a
    /// JVM package of the same name are separate ecosystems.
    #[test]
    fn b27_inline_mod_declarations_relative_requires_and_ecosystems() {
        use semantiq_parser::ImportContext;
        let rust = extract_file(
            "src/lib.rs",
            "mod top;\nmod outer {\n    mod child;\n    #[path = \"x.rs\"]\n    mod renamed;\n    \
             mod deeper { pub mod leaf; }\n}\n",
        );
        assert_eq!(
            rust.imports,
            vec![
                (
                    "top".to_string(),
                    Some("top".to_string()),
                    ImportKind::Local
                ),
                (
                    "self::outer::child".to_string(),
                    Some("child".to_string()),
                    ImportKind::Local
                ),
                (
                    "outer/x.rs".to_string(),
                    Some("renamed".to_string()),
                    ImportKind::Local
                ),
                (
                    "self::outer::deeper::leaf".to_string(),
                    Some("leaf".to_string()),
                    ImportKind::Local
                ),
            ]
        );

        let ruby = extract_file(
            "lib/app.rb",
            "require 'helpers'\nrequire_relative 'helpers'\nrequire_relative '../other'\n",
        );
        let paths: Vec<&str> = ruby.imports.iter().map(|(p, _, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["helpers", "./helpers", "../other"]);
        assert_eq!(ruby.imports[0].2, ImportKind::External);
        assert_eq!(ruby.imports[1].2, ImportKind::Local);

        let mut context = ImportContext {
            go_module: Some("example.com/myapp".to_string()),
            ..ImportContext::default()
        };
        context.add_source_file("myapp.go", Language::Go, "");
        context.add_source_file("cmd/app/main.go", Language::Go, "");
        context.add_source_file(
            "Models/User.cs",
            Language::CSharp,
            "namespace App.Models;\n",
        );
        context.add_source_file(
            "jvm/App/Services/Svc.java",
            Language::Java,
            "package App.Services;\n",
        );
        assert_eq!(context.go_package_dir("example.com/myapp"), Some(""));
        assert_eq!(
            context.resolve_go_import("example.com/myapp").as_deref(),
            Some("myapp.go")
        );
        assert_eq!(context.go_package_dir("example.com/myapplication"), None);
        let kinds = |path: &str, source: &str| {
            let lang = Language::from_path(Path::new(path)).unwrap();
            let mut support = LanguageSupport::new().unwrap();
            let tree = support.parse(lang, source).unwrap();
            ImportExtractor::extract_with_context(&tree, source, lang, &context)
                .unwrap()
                .into_iter()
                .map(|i| (i.path, i.kind))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(
                "Services/Svc.cs",
                "using App.Models;\nusing App.Services;\n"
            ),
            vec![
                ("App.Models".to_string(), ImportKind::Local),
                ("App.Services".to_string(), ImportKind::External),
            ]
        );
        assert_eq!(
            kinds(
                "jvm/App/A.java",
                "import App.Models.User;\nimport App.Services.Svc;\n"
            ),
            vec![
                ("App.Models.User".to_string(), ImportKind::External),
                ("App.Services.Svc".to_string(), ImportKind::Local),
            ]
        );
    }

    // ---- parser-symbols (#140 #145 #146 #148 #150 #154 #79 #144 #151) ----

    fn sym<'a>(symbols: &'a [Symbol], name: &str) -> &'a Symbol {
        symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol {name}: {symbols:?}"))
    }

    // B36 (#140): an annotated Java / C# / Kotlin / Scala / PHP member (and a
    // decorated TS class) gets the line that carries its name and modifiers
    // as signature, not the annotation; the span still starts on the
    // annotation.
    #[test]
    fn b36_annotated_member_signature_is_the_name_line() {
        let java = extract(
            Language::Java,
            "class C {\n    @Override\n    public double area() { return 1.0; }\n\n    @GetMapping(\"/x\")\n    @Transactional\n    public List<Point> list() { return null; }\n}\n",
        );
        let area = sym(&java, "area");
        assert_eq!(
            area.signature.as_deref(),
            Some("public double area() { return 1.0; }")
        );
        assert_eq!((area.start_line, area.end_line), (2, 3));
        assert_eq!(
            sym(&java, "list").signature.as_deref(),
            Some("public List<Point> list() { return null; }")
        );
        let cs = extract(
            Language::CSharp,
            "class C {\n    [HttpGet]\n    public string Get() { return \"\"; }\n}\n",
        );
        assert_eq!(
            sym(&cs, "Get").signature.as_deref(),
            Some("public string Get() { return \"\"; }")
        );
        let kt = extract(
            Language::Kotlin,
            "class A {\n    @Suppress(\"x\")\n    private fun hidden() {}\n}\n",
        );
        assert_eq!(
            sym(&kt, "hidden").signature.as_deref(),
            Some("private fun hidden() {}")
        );
        let scala = extract(
            Language::Scala,
            "class A {\n  @deprecated\n  private def old(): Int = 1\n}\n",
        );
        assert_eq!(
            sym(&scala, "old").signature.as_deref(),
            Some("private def old(): Int = 1")
        );
        let php = extract(
            Language::Php,
            "<?php\nclass Ctrl {\n    #[Route('/x')]\n    public function index() {}\n}\n",
        );
        assert_eq!(
            sym(&php, "index").signature.as_deref(),
            Some("public function index() {}")
        );
        let ts = extract(Language::TypeScript, "@Component()\nclass Service {}\n");
        assert_eq!(
            sym(&ts, "Service").signature.as_deref(),
            Some("class Service {}")
        );
        // A return type on its own line (GNU C style) joins the name line.
        let c = extract(Language::C, "static int\nmain(void) { return 0; }\n");
        assert_eq!(
            sym(&c, "main").signature.as_deref(),
            Some("static int main(void) { return 0; }")
        );
    }

    // B37 (#145): Go interface methods and `type X = Y` are symbols (and
    // definitions), methods carry their receiver type as parent, a grouped
    // import yields one short-named symbol per spec.
    #[test]
    fn b37_go_interface_methods_aliases_receivers_imports() {
        let source = "package main\n\nimport (\n\t\"fmt\"\n\t\"myapp/internal/store\"\n\ts \"strings\"\n)\n\ntype Shape interface {\n\tArea() float64\n}\n\ntype Circle struct{ r float64 }\n\ntype Alias = Circle\n\nfunc (c *Circle) Area() float64 { return c.r }\n";
        let e = extract_file("main.go", source);
        assert!(!e.has_error);
        let imports: Vec<(&str, usize)> = e
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .map(|s| (s.name.as_str(), s.start_line))
            .collect();
        assert_eq!(imports, vec![("fmt", 4), ("store", 5), ("s", 6)]);
        let methods: Vec<(&str, usize, Option<&str>)> = e
            .symbols
            .iter()
            .filter(|s| s.name == "Area")
            .map(|s| (s.name.as_str(), s.start_line, s.parent.as_deref()))
            .collect();
        assert_eq!(
            methods,
            vec![("Area", 10, Some("Shape")), ("Area", 17, Some("Circle"))]
        );
        assert!(
            e.symbols
                .iter()
                .all(|s| s.name != "Area" || s.kind == SymbolKind::Method)
        );
        let alias = sym(&e.symbols, "Alias");
        assert_eq!((alias.kind, alias.start_line), (SymbolKind::Type, 15));
        assert_eq!(
            kinds_of(&e.refs, "Area"),
            vec![
                (10, ReferenceKind::Definition),
                (17, ReferenceKind::Definition)
            ]
        );
        assert_eq!(
            kinds_of(&e.refs, "Alias"),
            vec![(15, ReferenceKind::Definition)]
        );
    }

    // B38 (#146): JS/TS class fields holding a function and `#private`
    // methods are method symbols; the calls in their body are attributed to
    // them, not to the class.
    #[test]
    fn b38_js_ts_function_fields_and_private_methods() {
        let js = "class A {\n  #count = 0;\n  #inc() { this.#count++; }\n  static #make() {}\n  run() { this.#inc(); A.#make(); }\n  onClick = (e) => { this.run(); };\n  static handler = function () {};\n  plain = 1;\n}\n";
        let e = extract_file("a.js", js);
        let methods: Vec<(&str, usize)> = e
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method)
            .map(|s| (s.name.as_str(), s.start_line))
            .collect();
        assert_eq!(
            methods,
            vec![
                ("#inc", 3),
                ("#make", 4),
                ("run", 5),
                ("onClick", 6),
                ("handler", 7)
            ]
        );
        assert!(e.symbols.iter().all(|s| s.name != "plain"));
        assert!(
            e.symbols
                .iter()
                .all(|s| s.parent.is_none() || s.parent.as_deref() == Some("A"))
        );
        assert_eq!(
            kinds_of(&e.refs, "onClick"),
            vec![(6, ReferenceKind::Definition)]
        );
        assert!(
            e.calls
                .contains(&("onClick".to_string(), "run".to_string(), 6))
        );
        assert!(
            e.calls
                .contains(&("run".to_string(), "#inc".to_string(), 5))
        );
        assert!(!e.calls.iter().any(|(caller, _, _)| caller == "A"));

        let ts = "class A {\n  #inc() {}\n  private onClick = (e: Event): void => { this.run(); };\n  static handler = function () {};\n  run() {}\n}\n";
        let e = extract_file("a.ts", ts);
        let methods: Vec<(&str, usize)> = e
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method)
            .map(|s| (s.name.as_str(), s.start_line))
            .collect();
        assert_eq!(
            methods,
            vec![("#inc", 2), ("onClick", 3), ("handler", 4), ("run", 5)]
        );
        assert_eq!(
            sym(&e.symbols, "onClick").signature.as_deref(),
            Some("private onClick = (e: Event): void => { this.run(); }")
        );
        assert!(
            e.calls
                .contains(&("onClick".to_string(), "run".to_string(), 3))
        );
    }

    // B39 (#148): C `#define` macros and function-pointer typedefs are
    // symbols; macro names and enumerators are definitions, not usages.
    #[test]
    fn b39_c_macros_function_pointer_typedefs_enumerators() {
        let source = "#define MAX_BUF 10\n#define SQUARE(x) ((x) * (x))\ntypedef enum { RED, GREEN } color_t;\ntypedef int (*callback_t)(int);\nenum State { IDLE, BUSY };\nint main(void) { return SQUARE(MAX_BUF) + RED; }\n";
        let e = extract_file("a.c", source);
        assert!(!e.has_error);
        let max = sym(&e.symbols, "MAX_BUF");
        assert_eq!((max.kind, max.start_line), (SymbolKind::Constant, 1));
        let square = sym(&e.symbols, "SQUARE");
        assert_eq!((square.kind, square.start_line), (SymbolKind::Function, 2));
        assert_eq!(
            square.signature.as_deref(),
            Some("#define SQUARE(x) ((x) * (x))")
        );
        let cb = sym(&e.symbols, "callback_t");
        assert_eq!((cb.kind, cb.start_line), (SymbolKind::Type, 4));
        assert_eq!(
            kinds_of(&e.refs, "MAX_BUF"),
            vec![
                (1, ReferenceKind::Definition),
                (6, ReferenceKind::Reference)
            ]
        );
        assert_eq!(
            kinds_of(&e.refs, "SQUARE"),
            vec![(2, ReferenceKind::Definition), (6, ReferenceKind::Call)]
        );
        assert_eq!(
            kinds_of(&e.refs, "RED"),
            vec![
                (3, ReferenceKind::Definition),
                (6, ReferenceKind::Reference)
            ]
        );
        assert_eq!(
            kinds_of(&e.refs, "IDLE"),
            vec![(5, ReferenceKind::Definition)]
        );
        // Same patterns in the C++ query (headers are parsed with it).
        let h = extract_file("a.h", source);
        assert_eq!(sym(&h.symbols, "MAX_BUF").kind, SymbolKind::Constant);
        assert_eq!(sym(&h.symbols, "callback_t").kind, SymbolKind::Type);
    }

    // B40 (#150): Python docstrings and Elixir `@doc` / `@moduledoc` are the
    // symbol's documentation.
    #[test]
    fn b40_python_docstrings_and_elixir_doc_attributes() {
        let py = extract(
            Language::Python,
            concat!(
                "def f():\n    \"\"\"Doc of f.\"\"\"\n    return 1\n\n",
                "class C:\n    \"\"\"Doc of C.\n\n    More:\n        indented\n    \"\"\"\n",
                "    def m(self):\n        '''Doc of m.'''\n\n",
                "    def n(self):\n        x = 'not a docstring'\n",
            ),
        );
        assert_eq!(sym(&py, "f").doc_comment.as_deref(), Some("Doc of f."));
        assert_eq!(
            sym(&py, "C").doc_comment.as_deref(),
            Some("Doc of C.\n\nMore:\n    indented")
        );
        assert_eq!(sym(&py, "m").doc_comment.as_deref(), Some("Doc of m."));
        assert_eq!(sym(&py, "n").doc_comment, None);

        let ex = extract(
            Language::Elixir,
            concat!(
                "defmodule Geo do\n  @moduledoc \"Geometry helpers\"\n\n",
                "  @doc \"area of a shape\"\n  def area(%{r: r}), do: r\n\n",
                "  @doc \"\"\"\n  Multi-line doc.\n  \"\"\"\n  @spec name(any) :: String.t()\n  def name(_), do: \"x\"\n\n",
                "  @doc false\n  def hidden, do: 1\n\n  def undocumented, do: 1\nend\n",
            ),
        );
        assert_eq!(
            sym(&ex, "Geo").doc_comment.as_deref(),
            Some("Geometry helpers")
        );
        assert_eq!(
            sym(&ex, "area").doc_comment.as_deref(),
            Some("area of a shape")
        );
        assert_eq!(
            sym(&ex, "name").doc_comment.as_deref(),
            Some("Multi-line doc.")
        );
        assert_eq!(sym(&ex, "hidden").doc_comment, None);
        assert_eq!(sym(&ex, "undocumented").doc_comment, None);
    }

    // B41 (#154): HTML `<script>` / `<style>` and elements with an `id` are
    // symbols at any depth; anonymous elements are not.
    #[test]
    fn b41_html_nested_ids_scripts_styles() {
        let source = "<html>\n<head><style>body {}</style></head>\n<body>\n  <div id=\"main\" class=\"c\">\n    <p>text</p>\n    <section id=\"about\"><h2>About</h2></section>\n  </div>\n  <script>var x = 1;</script>\n</body>\n</html>\n";
        let symbols = extract(Language::Html, source);
        let found: Vec<(&str, SymbolKind, usize)> = symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.start_line))
            .collect();
        assert_eq!(
            found,
            vec![
                ("html", SymbolKind::Variable, 1),
                ("style", SymbolKind::Module, 2),
                ("main", SymbolKind::Variable, 4),
                ("about", SymbolKind::Variable, 6),
                ("script", SymbolKind::Module, 8),
            ]
        );
        assert_eq!(
            sym(&symbols, "main").signature.as_deref(),
            Some(r#"<div id="main" class="c">"#)
        );
    }

    // B42 (#79): a C++ method defined out of its class is indexed under its
    // bare name with the qualifier as parent (joined to the enclosing
    // namespace); its definition line is a definition, not a usage; the
    // in-class declaration is a method symbol too.
    #[test]
    fn b42_cpp_out_of_class_method_bare_name() {
        let source = "namespace ns {\nclass C { int add(int); ~C(); };\nint C::add(int x) { return helper(x); }\nC::~C() {}\n}\nint ns::C::sub(int x) { return x; }\nint main() { ns::C c; return c.add(1); }\n";
        let e = extract_file("c.cpp", source);
        assert!(!e.has_error);
        let methods: Vec<(&str, usize, Option<&str>)> = e
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method)
            .map(|s| (s.name.as_str(), s.start_line, s.parent.as_deref()))
            .collect();
        assert_eq!(
            methods,
            vec![
                ("add", 2, Some("ns::C")),
                ("~C", 2, Some("ns::C")),
                ("add", 3, Some("ns::C")),
                ("~C", 4, Some("ns::C")),
                ("sub", 6, Some("ns::C")),
            ]
        );
        assert!(e.symbols.iter().all(|s| !s.name.contains("::")));
        assert_eq!(
            kinds_of(&e.refs, "add"),
            vec![
                (2, ReferenceKind::Definition),
                (3, ReferenceKind::Definition),
                (7, ReferenceKind::Call)
            ]
        );
        assert_eq!(
            kinds_of(&e.refs, "sub"),
            vec![(6, ReferenceKind::Definition)]
        );
        assert!(
            e.calls
                .contains(&("add".to_string(), "helper".to_string(), 3))
        );
    }

    // B43 (#144): a `.h` header is parsed with the C++ grammar: classes,
    // namespaces, templates and member declarations are symbols, keywords are
    // not references; `.c` keeps the C grammar.
    #[test]
    fn b43_h_header_parsed_as_cpp() {
        let header = "namespace geo {\nclass Shape {\n public:\n  virtual double area() const = 0;\n  void describe();\n private:\n  int id_;\n};\ntemplate <typename T>\nstruct Box : public Shape {\n  double area() const override { return 0; }\n};\n}\n";
        let e = extract_file("inc/shape.h", header);
        assert!(!e.has_error);
        let found: Vec<(&str, SymbolKind, Option<&str>)> = e
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.parent.as_deref()))
            .collect();
        assert_eq!(
            found,
            vec![
                ("geo", SymbolKind::Module, None),
                ("Shape", SymbolKind::Class, Some("geo")),
                ("area", SymbolKind::Method, Some("geo::Shape")),
                ("describe", SymbolKind::Method, Some("geo::Shape")),
                ("Box", SymbolKind::Struct, Some("geo")),
                ("area", SymbolKind::Method, Some("geo::Box")),
            ]
        );
        for keyword in [
            "class",
            "namespace",
            "public",
            "virtual",
            "template",
            "override",
        ] {
            assert!(
                e.refs.iter().all(|r| r.name != keyword),
                "{keyword} is a reference"
            );
        }
        assert_eq!(
            Language::from_path(Path::new("inc/shape.h")),
            Some(Language::C)
        );
        // A C header still works, and a `.c` file is parsed with the C grammar.
        let c = extract_file("inc/util.h", "#include <stdio.h>\nint add(int a, int b);\n");
        assert!(!c.has_error);
        assert_eq!(c.imports.len(), 1);
        let c = extract_file("src/util.c", "int add(int a, int b) { return a + b; }\n");
        assert!(!c.has_error);
        assert_eq!(sym(&c.symbols, "add").kind, SymbolKind::Function);
    }

    // B44 (#151): `.mts` / `.cts` files are TypeScript.
    #[test]
    fn b44_mts_cts_are_typescript() {
        assert_eq!(
            Language::from_path(Path::new("src/a.mts")),
            Some(Language::TypeScript)
        );
        assert_eq!(
            Language::from_path(Path::new("src/a.cts")),
            Some(Language::TypeScript)
        );
        let e = extract_file(
            "src/a.mts",
            "import { x } from './foo.mjs';\nexport const run = (): number => x;\n",
        );
        assert!(!e.has_error);
        assert_eq!(sym(&e.symbols, "run").kind, SymbolKind::Function);
        assert_eq!(e.imports.len(), 1);
    }
}
