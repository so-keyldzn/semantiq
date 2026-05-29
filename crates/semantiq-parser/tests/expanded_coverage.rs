// Audit probes — vérifient que les nouvelles captures introduites par
// l'élargissement des queries fonctionnent comme attendu sur les vrais
// parsers tree-sitter. À supprimer si déplacé dans la suite normale.

use semantiq_parser::language::{Language, LanguageSupport};
use semantiq_parser::query_extractor::QuerySymbolExtractor;
use semantiq_parser::symbols::SymbolKind;

fn run(lang: Language, src: &str) -> Vec<(String, SymbolKind, Option<String>)> {
    let mut support = LanguageSupport::new().unwrap();
    let extractor = QuerySymbolExtractor::new().unwrap();
    let tree = support.parse(lang, src).unwrap();
    extractor
        .extract(&tree, src, lang)
        .unwrap()
        .into_iter()
        .map(|s| (s.name, s.kind, s.parent))
        .collect()
}

#[test]
fn audit_python_constants() {
    let src = "import os\n\nPI = 3.14\nDEBUG = True\n__all__ = [\"a\"]\n\ndef foo(): pass\n\nx = compute()\n";
    let symbols = run(Language::Python, src);
    eprintln!("PYTHON: {symbols:?}");
    let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(names.contains(&"PI"), "PI missed: {names:?}");
    assert!(names.contains(&"DEBUG"), "DEBUG missed: {names:?}");
    assert!(names.contains(&"x"), "x assignment missed: {names:?}");
}

#[test]
fn audit_ruby_alias() {
    let src = "class Foo\n  def old_name; end\n  alias new_name old_name\nend\n";
    let symbols = run(Language::Ruby, src);
    eprintln!("RUBY: {symbols:?}");
    let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(
        names.contains(&"new_name"),
        "alias new name missed: {names:?}"
    );
    // Si on ne capture QUE old_name, c'est un bug — on indexe le mauvais nom.
}

#[test]
fn audit_go_aliases() {
    let src = "package main\n\ntype MyInt int\ntype Handler = func()\ntype Foo struct { X int }\ntype Bar interface { Run() }\n";
    let symbols = run(Language::Go, src);
    eprintln!("GO: {symbols:?}");
    let pairs: Vec<(&str, &SymbolKind)> = symbols.iter().map(|(n, k, _)| (n.as_str(), k)).collect();
    assert!(
        pairs
            .iter()
            .any(|(n, k)| *n == "Foo" && matches!(k, SymbolKind::Struct)),
        "Foo as Struct missed: {pairs:?}"
    );
    assert!(
        pairs
            .iter()
            .any(|(n, k)| *n == "Bar" && matches!(k, SymbolKind::Interface)),
        "Bar as Interface missed: {pairs:?}"
    );
    assert!(
        pairs
            .iter()
            .any(|(n, k)| *n == "MyInt" && matches!(k, SymbolKind::Type)),
        "MyInt as Type missed: {pairs:?}"
    );
}

#[test]
fn audit_csharp_qualified_namespace_and_record() {
    let src = "namespace A.B.C {\n    class Foo {}\n}\npublic record Person(string Name);\npublic delegate void Handler(int x);\n";
    let symbols = run(Language::CSharp, src);
    eprintln!("CSHARP: {symbols:?}");
    assert!(
        symbols
            .iter()
            .any(|(n, k, _)| n == "A.B.C" && matches!(k, SymbolKind::Module)),
        "Qualified namespace missed: {symbols:?}"
    );
    let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(names.contains(&"Person"), "record missed: {names:?}");
    assert!(names.contains(&"Handler"), "delegate missed: {names:?}");
}

#[test]
fn audit_rust_trait_methods() {
    let src = "trait Foo {\n    fn bar(&self);\n    fn baz(&self) { println!(\"d\"); }\n}\n";
    let symbols = run(Language::Rust, src);
    eprintln!("RUST: {symbols:?}");
    let bar = symbols.iter().find(|(n, _, _)| n == "bar").unwrap();
    assert!(
        matches!(bar.1, SymbolKind::Method),
        "bar should be Method, got {:?}",
        bar.1
    );
}

#[test]
fn audit_js_object_methods() {
    let src = "const obj = {\n  foo: () => {},\n  bar: function() {},\n  baz() {}\n};\nconst Cls = class { hi() {} };\n";
    let symbols = run(Language::JavaScript, src);
    eprintln!("JS: {symbols:?}");
    let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(names.contains(&"foo"), "foo arrow missed: {names:?}");
    assert!(
        names.contains(&"bar"),
        "bar function_expression missed: {names:?}"
    );
    assert!(names.contains(&"Cls"), "anonymous class missed: {names:?}");
}

#[test]
fn audit_kotlin() {
    let src = "typealias Handler = (Int) -> String\nclass Foo {\n  companion object {\n    fun create(): Foo = Foo()\n  }\n}\nenum class Color { RED, GREEN, BLUE }\n";
    let symbols = run(Language::Kotlin, src);
    eprintln!("KOTLIN: {symbols:?}");
    let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(names.contains(&"Handler"), "typealias missed: {names:?}");
    assert!(
        names.contains(&"create"),
        "companion method missed: {names:?}"
    );
    assert!(names.contains(&"RED"), "enum entry missed: {names:?}");
}

#[test]
fn audit_elixir() {
    let src = "defmodule Foo do\n  def bar(x) when is_integer(x), do: x\nend\n\ndefprotocol Stringify do\n  def to_string(value)\nend\n";
    let symbols = run(Language::Elixir, src);
    eprintln!("ELIXIR: {symbols:?}");
    let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(
        names.contains(&"Stringify"),
        "defprotocol missed: {names:?}"
    );
    assert!(names.contains(&"bar"), "when guard missed: {names:?}");
}

#[test]
fn audit_typescript() {
    let src = "interface Repo {\n  fetch(id: string): Promise<void>;\n  count: number;\n}\n\nnamespace Utils {\n  export function noop(): void {}\n}\ndeclare function ambient(): void;\n";
    let symbols = run(Language::TypeScript, src);
    eprintln!("TS: {symbols:?}");
    let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(
        names.contains(&"fetch"),
        "interface method missed: {names:?}"
    );
    assert!(names.contains(&"Utils"), "namespace missed: {names:?}");
    assert!(
        names.contains(&"ambient"),
        "ambient declare missed: {names:?}"
    );
}

#[test]
fn audit_toml_dotted_pair() {
    let src = "server.host = \"localhost\"\nserver.port = 8080\n[db]\nuser = \"x\"\n";
    let symbols = run(Language::Toml, src);
    eprintln!("TOML: {symbols:?}");
    let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(
        names.contains(&"server.host"),
        "dotted pair missed: {names:?}"
    );
}

#[test]
fn audit_js_multi_declarator_const() {
    // `const A = 1, B = 2, C = 3;` : un seul `lexical_declaration` (@definition)
    // porte trois `variable_declarator` → trois @name distincts. Avant le fix de
    // la clé de dédup (qui n'utilisait que la plage du @definition), seul A était
    // indexé. Désormais la plage du nœud de NOM fait partie de la clé.
    let src = "const A = 1, B = 2, C = 3;\nlet x = 10, y = 20;\n";
    let symbols = run(Language::JavaScript, src);
    eprintln!("JS multi-decl: {symbols:?}");
    let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_str()).collect();
    for expected in ["A", "B", "C", "x", "y"] {
        assert!(
            names.contains(&expected),
            "multi-declarator name '{expected}' missed: {names:?}"
        );
    }
}

#[test]
fn audit_ts_multi_declarator_const() {
    // Idem pour TypeScript : `const A = 1, B = 2;` doit produire deux symboles.
    let src = "const A = 1, B = 2, C = 3;\nlet p = 1, q = 2;\n";
    let symbols = run(Language::TypeScript, src);
    eprintln!("TS multi-decl: {symbols:?}");
    let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_str()).collect();
    for expected in ["A", "B", "C", "p", "q"] {
        assert!(
            names.contains(&expected),
            "multi-declarator name '{expected}' missed: {names:?}"
        );
    }
}

#[test]
fn audit_scala() {
    let src = "package com.example.app\n\ntrait Repo {\n  val name: String\n  def fetch(): String\n}\n\nenum Color { case Red, Green }\n";
    let symbols = run(Language::Scala, src);
    eprintln!("SCALA: {symbols:?}");
    let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(
        names.iter().any(|n| n.contains("com.example.app")),
        "package missed: {names:?}"
    );
    assert!(names.contains(&"name"), "abstract val missed: {names:?}");
    assert!(
        names.contains(&"Red") || names.contains(&"Green"),
        "enum case missed: {names:?}"
    );
}

#[test]
fn audit_scala_multi_binding_val() {
    // `val a, b = 1` : le grammar regroupe a et b dans un nœud `identifiers`.
    // Les deux noms doivent être indexés (régression : seul `a` l'était).
    let src = "object M {\n  val a, b = 1\n  var x, y, z = 0\n}\n";
    let symbols = run(Language::Scala, src);
    eprintln!("SCALA multi-binding: {symbols:?}");
    let names: Vec<&str> = symbols.iter().map(|(n, _, _)| n.as_str()).collect();
    for expected in ["a", "b", "x", "y", "z"] {
        assert!(
            names.contains(&expected),
            "multi-binding val/var name '{expected}' missed: {names:?}"
        );
    }
}
