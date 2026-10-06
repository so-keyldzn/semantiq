//! Structural relations extracted from the AST: call edges and type hierarchy.
//!
//! - **Call edges** attach every `call` reference to its caller, the innermost
//!   function or method whose span contains the call line. They are derived
//!   from the symbols and references already extracted, so they work for every
//!   code language.
//! - **Type relations** record "implements / extends" declarations: Rust
//!   `impl Trait for Type` and supertraits, TypeScript/JavaScript, Python,
//!   Java, Kotlin, C#, C++, PHP, Ruby and Scala class headers. Go interfaces
//!   are satisfied implicitly (no syntax names them), so Go has none.
//!
//! As for references, names are stored without qualification or generics
//! (`std::fmt::Display<T>` → `Display`) and resolution is by name only.

use crate::language::Language;
use crate::references::{Reference, ReferenceKind};
use crate::symbols::{Symbol, SymbolKind};
use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Tree};

/// One call site attached to its caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallEdge {
    /// Enclosing function/method (or type, for field initialisers). Empty for
    /// top-level code.
    pub caller: String,
    /// Start line of the caller's definition (0 for top-level code).
    pub caller_line: usize,
    pub callee: String,
    /// Line of the call (1-based).
    pub line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// Class inheritance, interface/trait extension (supertraits).
    Extends,
    /// Interface implementation, `impl Trait for Type`, mixins.
    Implements,
}

impl RelationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RelationKind::Extends => "extends",
            RelationKind::Implements => "implements",
        }
    }
}

/// `type_name` extends / implements `super_name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeRelation {
    pub type_name: String,
    pub super_name: String,
    pub kind: RelationKind,
    /// Line of the declaring node (class header, `impl` block), 1-based.
    pub line: usize,
    /// Last line of the declaring node: for Rust, the methods of an
    /// `impl Trait for Type` block are the ones inside `line..=end_line`.
    pub end_line: usize,
}

/// Call edges and type relations of one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileStructure {
    pub calls: Vec<CallEdge>,
    pub relations: Vec<TypeRelation>,
}

pub struct StructureExtractor;

impl StructureExtractor {
    pub fn extract(
        tree: &Tree,
        source: &str,
        language: Language,
        symbols: &[Symbol],
        references: &[Reference],
    ) -> FileStructure {
        FileStructure {
            calls: Self::call_edges(symbols, references),
            relations: Self::type_relations(tree, source, language),
        }
    }

    /// Attach each `call` reference to the innermost function/method containing
    /// it, falling back to the innermost type (field initialisers, class
    /// bodies), then to top-level code.
    pub fn call_edges(symbols: &[Symbol], references: &[Reference]) -> Vec<CallEdge> {
        let innermost = |line: usize, pred: &dyn Fn(SymbolKind) -> bool| {
            symbols
                .iter()
                .filter(|s| pred(s.kind) && s.start_line <= line && line <= s.end_line)
                .min_by_key(|s| s.end_line - s.start_line)
        };

        references
            .iter()
            .filter(|r| r.kind == ReferenceKind::Call)
            .map(|r| {
                let caller = innermost(r.line, &|k| {
                    matches!(k, SymbolKind::Function | SymbolKind::Method)
                })
                .or_else(|| {
                    innermost(r.line, &|k| {
                        !matches!(
                            k,
                            SymbolKind::Import | SymbolKind::Module | SymbolKind::Variable
                        )
                    })
                });
                CallEdge {
                    caller: caller.map(|s| s.name.clone()).unwrap_or_default(),
                    caller_line: caller.map(|s| s.start_line).unwrap_or(0),
                    callee: r.name.clone(),
                    line: r.line,
                }
            })
            .collect()
    }

    pub fn type_relations(tree: &Tree, source: &str, language: Language) -> Vec<TypeRelation> {
        let mut relations = Vec::new();
        let mut stack = vec![tree.root_node()];
        let mut cursor = tree.walk();
        while let Some(node) = stack.pop() {
            relations_of(node, source.as_bytes(), language, &mut relations);
            stack.extend(node.named_children(&mut cursor));
        }
        relations.retain(|r| r.type_name != r.super_name);
        relations.sort_by(|a, b| {
            a.line
                .cmp(&b.line)
                .then_with(|| a.super_name.cmp(&b.super_name))
        });
        relations.dedup();
        relations
    }
}

/// Bare type name: generics, call arguments and qualification removed.
/// `std::fmt::Display` → `Display`, `Base<T>` → `Base`, `mod.B()` → `B`.
fn bare_name(node: Node, src: &[u8]) -> Option<String> {
    let text = node.utf8_text(src).ok()?;
    let head = text.split(['<', '[', '(', '{']).next()?.trim();
    let last = head
        .rsplit(['.', ':', '\\'])
        .next()
        .map(str::trim)
        .unwrap_or(head);
    let valid = !last.is_empty()
        && last
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    valid.then(|| last.to_string())
}

fn named_children<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn child_of_kind<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    named_children(node)
        .into_iter()
        .find(|c| kinds.contains(&c.kind()))
}

/// Supertype candidates: skips comments, access specifiers and keyword args.
fn type_children(node: Node) -> Vec<Node> {
    named_children(node)
        .into_iter()
        .filter(|c| {
            !matches!(
                c.kind(),
                "comment"
                    | "line_comment"
                    | "block_comment"
                    | "access_specifier"
                    | "keyword_argument"
                    | "lifetime"
                    | "type_arguments"
                    | "arguments"
                    | "value_arguments"
            )
        })
        .collect()
}

/// Name declared by a class-like node: its `name` field, or the variable it
/// is assigned to (`const X = class extends Y {}`).
fn declared_name(node: Node, src: &[u8]) -> Option<String> {
    if let Some(name) = node.child_by_field_name("name") {
        return bare_name(name, src);
    }
    let parent = node.parent()?;
    if parent.kind() == "variable_declarator" {
        return bare_name(parent.child_by_field_name("name")?, src);
    }
    None
}

fn relations_of(node: Node, src: &[u8], language: Language, out: &mut Vec<TypeRelation>) {
    let line = node.start_position().row + 1;
    let end_line = node.end_position().row + 1;
    let mut push = |type_name: &str, super_node: Node, kind: RelationKind| {
        if let Some(super_name) = bare_name(super_node, src) {
            out.push(TypeRelation {
                type_name: type_name.to_string(),
                super_name,
                kind,
                line,
                end_line,
            });
        }
    };
    let kind = node.kind();

    match language {
        Language::Rust => match kind {
            "impl_item" => {
                if let (Some(trait_node), Some(type_node)) = (
                    node.child_by_field_name("trait"),
                    node.child_by_field_name("type"),
                ) && let Some(type_name) = bare_name(type_node, src)
                {
                    push(&type_name, trait_node, RelationKind::Implements);
                }
            }
            "trait_item" => {
                if let (Some(name), Some(bounds)) =
                    (declared_name(node, src), node.child_by_field_name("bounds"))
                {
                    for bound in type_children(bounds) {
                        push(&name, bound, RelationKind::Extends);
                    }
                }
            }
            _ => {}
        },
        Language::TypeScript | Language::JavaScript => match kind {
            "class_declaration" | "abstract_class_declaration" | "class" => {
                let (Some(name), Some(heritage)) = (
                    declared_name(node, src),
                    child_of_kind(node, &["class_heritage"]),
                ) else {
                    return;
                };
                for clause in named_children(heritage) {
                    match clause.kind() {
                        "extends_clause" => {
                            for value in type_children(clause) {
                                push(&name, value, RelationKind::Extends);
                            }
                        }
                        "implements_clause" => {
                            for value in type_children(clause) {
                                push(&name, value, RelationKind::Implements);
                            }
                        }
                        // JavaScript: `class A extends B` has no clause node.
                        _ => push(&name, clause, RelationKind::Extends),
                    }
                }
            }
            "interface_declaration" => {
                if let (Some(name), Some(clause)) = (
                    declared_name(node, src),
                    child_of_kind(node, &["extends_type_clause"]),
                ) {
                    for value in type_children(clause) {
                        push(&name, value, RelationKind::Extends);
                    }
                }
            }
            _ => {}
        },
        Language::Python => {
            if kind == "class_definition"
                && let (Some(name), Some(bases)) = (
                    declared_name(node, src),
                    node.child_by_field_name("superclasses"),
                )
            {
                for base in type_children(bases) {
                    if base.utf8_text(src).ok() != Some("object") {
                        push(&name, base, RelationKind::Extends);
                    }
                }
            }
        }
        Language::Java => {
            let Some(name) = declared_name(node, src) else {
                return;
            };
            match kind {
                "class_declaration" | "enum_declaration" | "record_declaration" => {
                    if let Some(superclass) = node.child_by_field_name("superclass") {
                        for value in type_children(superclass) {
                            push(&name, value, RelationKind::Extends);
                        }
                    }
                    if let Some(interfaces) = node.child_by_field_name("interfaces") {
                        for list in named_children(interfaces) {
                            for value in type_children(list) {
                                push(&name, value, RelationKind::Implements);
                            }
                        }
                    }
                }
                "interface_declaration" => {
                    if let Some(extends) = child_of_kind(node, &["extends_interfaces"]) {
                        for list in named_children(extends) {
                            for value in type_children(list) {
                                push(&name, value, RelationKind::Extends);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Language::Kotlin => {
            if !matches!(kind, "class_declaration" | "object_declaration") {
                return;
            }
            let (Some(name), Some(specifiers)) = (
                declared_name(node, src),
                child_of_kind(node, &["delegation_specifiers"]),
            ) else {
                return;
            };
            let is_interface = node
                .child(0)
                .is_some_and(|c| c.utf8_text(src).ok() == Some("interface"));
            for specifier in named_children(specifiers) {
                // `B()` calls a superclass constructor; a bare type is an
                // interface (or a superclass declared elsewhere).
                let (target, relation) = match child_of_kind(specifier, &["constructor_invocation"])
                {
                    Some(call) => (call, RelationKind::Extends),
                    None if is_interface => (specifier, RelationKind::Extends),
                    None => (specifier, RelationKind::Implements),
                };
                push(&name, target, relation);
            }
        }
        Language::CSharp => {
            if !matches!(
                kind,
                "class_declaration"
                    | "struct_declaration"
                    | "interface_declaration"
                    | "record_declaration"
            ) {
                return;
            }
            let (Some(name), Some(bases)) = (
                declared_name(node, src),
                child_of_kind(node, &["base_list"]),
            ) else {
                return;
            };
            for (i, base) in type_children(bases).into_iter().enumerate() {
                // The syntax does not tell a base class from an interface: only
                // the first entry of a class can be a class, and by convention
                // interface names are `I` + uppercase letter.
                let base_class = kind != "struct_declaration"
                    && i == 0
                    && !bare_name(base, src).is_some_and(|n| looks_like_interface(&n));
                let relation = if kind == "interface_declaration" || base_class {
                    RelationKind::Extends
                } else {
                    RelationKind::Implements
                };
                push(&name, base, relation);
            }
        }
        Language::Cpp => {
            if matches!(kind, "class_specifier" | "struct_specifier")
                && let (Some(name), Some(bases)) = (
                    declared_name(node, src),
                    child_of_kind(node, &["base_class_clause"]),
                )
            {
                for base in type_children(bases) {
                    push(&name, base, RelationKind::Extends);
                }
            }
        }
        Language::Php => {
            if !matches!(kind, "class_declaration" | "interface_declaration") {
                return;
            }
            let Some(name) = declared_name(node, src) else {
                return;
            };
            for clause in named_children(node) {
                let relation = match clause.kind() {
                    "base_clause" => RelationKind::Extends,
                    "class_interface_clause" => RelationKind::Implements,
                    _ => continue,
                };
                for value in type_children(clause) {
                    push(&name, value, relation);
                }
            }
        }
        Language::Ruby => {
            if kind == "class"
                && let (Some(name), Some(superclass)) = (
                    declared_name(node, src),
                    node.child_by_field_name("superclass"),
                )
            {
                for value in type_children(superclass) {
                    push(&name, value, RelationKind::Extends);
                }
            }
        }
        Language::Scala => {
            if !matches!(
                kind,
                "class_definition" | "trait_definition" | "object_definition"
            ) {
                return;
            }
            let (Some(name), Some(extends)) =
                (declared_name(node, src), node.child_by_field_name("extend"))
            else {
                return;
            };
            // `extends A with B with C`: A is the superclass, the rest mixins.
            for (i, value) in type_children(extends).into_iter().enumerate() {
                let relation = if i == 0 {
                    RelationKind::Extends
                } else {
                    RelationKind::Implements
                };
                push(&name, value, relation);
            }
        }
        // Go interfaces are implicit; C, Bash, Elixir and data files have no
        // inheritance syntax.
        _ => {}
    }
}

fn looks_like_interface(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next() == Some('I') && chars.next().is_some_and(|c| c.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LanguageSupport, ReferenceExtractor, SymbolExtractor};

    fn relations(language: Language, source: &str) -> Vec<(String, String, &'static str)> {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(language, source).unwrap();
        StructureExtractor::type_relations(&tree, source, language)
            .into_iter()
            .map(|r| (r.type_name, r.super_name, r.kind.as_str()))
            .collect()
    }

    fn rel(t: &str, s: &str, k: &'static str) -> (String, String, &'static str) {
        (t.to_string(), s.to_string(), k)
    }

    fn edges(language: Language, source: &str) -> Vec<(String, String, usize)> {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(language, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, language).unwrap();
        let refs = ReferenceExtractor::extract(&tree, source, language);
        StructureExtractor::extract(&tree, source, language, &symbols, &refs)
            .calls
            .into_iter()
            .map(|e| (e.caller, e.callee, e.line))
            .collect()
    }

    fn edge(caller: &str, callee: &str, line: usize) -> (String, String, usize) {
        (caller.to_string(), callee.to_string(), line)
    }

    #[test]
    fn test_rust_relations() {
        let r = relations(
            Language::Rust,
            "impl<T> fmt::Display for Foo<T> {}\nimpl Bar {}\ntrait A: B + std::fmt::Debug {}",
        );
        assert_eq!(
            r,
            vec![
                rel("Foo", "Display", "implements"),
                rel("A", "B", "extends"),
                rel("A", "Debug", "extends"),
            ]
        );
    }

    #[test]
    fn test_rust_impl_span() {
        let src = "struct S;\nimpl Tr for S {\n    fn a() {}\n}\n";
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(Language::Rust, src).unwrap();
        let r = StructureExtractor::type_relations(&tree, src, Language::Rust);
        assert_eq!((r[0].line, r[0].end_line), (2, 4));
    }

    #[test]
    fn test_typescript_relations() {
        let r = relations(
            Language::TypeScript,
            "class A extends B<T> implements C, D.E {}\ninterface I extends J, K<X> {}\nabstract class Z extends Y {}",
        );
        assert_eq!(
            r,
            vec![
                rel("A", "B", "extends"),
                rel("A", "C", "implements"),
                rel("A", "E", "implements"),
                rel("I", "J", "extends"),
                rel("I", "K", "extends"),
                rel("Z", "Y", "extends"),
            ]
        );
    }

    #[test]
    fn test_javascript_relations() {
        let r = relations(
            Language::JavaScript,
            "class A extends mod.B {}\nconst X = class extends Y {}",
        );
        assert_eq!(r, vec![rel("A", "B", "extends"), rel("X", "Y", "extends")]);
    }

    #[test]
    fn test_python_relations() {
        let r = relations(
            Language::Python,
            "class A(B, mod.C, metaclass=M):\n    pass\nclass O(object):\n    pass\n",
        );
        assert_eq!(r, vec![rel("A", "B", "extends"), rel("A", "C", "extends")]);
    }

    #[test]
    fn test_java_relations() {
        let r = relations(
            Language::Java,
            "class A extends B<T> implements C, D {}\ninterface I extends J, K {}\nenum E implements F {}",
        );
        assert_eq!(
            r,
            vec![
                rel("A", "B", "extends"),
                rel("A", "C", "implements"),
                rel("A", "D", "implements"),
                rel("I", "J", "extends"),
                rel("I", "K", "extends"),
                rel("E", "F", "implements"),
            ]
        );
    }

    #[test]
    fn test_kotlin_relations() {
        let r = relations(
            Language::Kotlin,
            "class A : B(), C, D<T> {}\ninterface I : J\nobject O : I {}",
        );
        assert_eq!(
            r,
            vec![
                rel("A", "B", "extends"),
                rel("A", "C", "implements"),
                rel("A", "D", "implements"),
                rel("I", "J", "extends"),
                rel("O", "I", "implements"),
            ]
        );
    }

    #[test]
    fn test_csharp_relations() {
        let r = relations(
            Language::CSharp,
            "class A : B, IC, D<T> {}\nclass P : IQ {}\ninterface I : IJ {}\nstruct S : IK {}",
        );
        assert_eq!(
            r,
            vec![
                rel("A", "B", "extends"),
                rel("A", "D", "implements"),
                rel("A", "IC", "implements"),
                rel("P", "IQ", "implements"),
                rel("I", "IJ", "extends"),
                rel("S", "IK", "implements"),
            ]
        );
    }

    #[test]
    fn test_other_languages_relations() {
        assert_eq!(
            relations(Language::Cpp, "class A : public B, private ns::C<T> {};"),
            vec![rel("A", "B", "extends"), rel("A", "C", "extends")]
        );
        assert_eq!(
            relations(
                Language::Php,
                "<?php\nclass A extends B implements C, D {}\ninterface I extends J {}"
            ),
            vec![
                rel("A", "B", "extends"),
                rel("A", "C", "implements"),
                rel("A", "D", "implements"),
                rel("I", "J", "extends"),
            ]
        );
        assert_eq!(
            relations(Language::Ruby, "class A < B\nend\nclass C < M::D\nend"),
            vec![rel("A", "B", "extends"), rel("C", "D", "extends")]
        );
        assert_eq!(
            relations(Language::Scala, "class A extends B(1) with C[T]"),
            vec![rel("A", "B", "extends"), rel("A", "C", "implements")]
        );
    }

    #[test]
    fn test_go_has_no_relations() {
        assert!(
            relations(
                Language::Go,
                "package p\ntype R interface { Read() }\ntype F struct{}\nfunc (f F) Read() {}"
            )
            .is_empty()
        );
    }

    #[test]
    fn test_rust_call_edges() {
        let e = edges(
            Language::Rust,
            "fn ping() {\n    pong();\n}\nfn pong() {\n    ping();\n    x.helper();\n}\nstruct S;\nimpl S {\n    fn meth(&self) {\n        ping();\n    }\n}\n",
        );
        assert_eq!(
            e,
            vec![
                edge("ping", "pong", 2),
                edge("pong", "ping", 5),
                edge("pong", "helper", 6),
                edge("meth", "ping", 11),
            ]
        );
    }

    #[test]
    fn test_python_call_edges_top_level() {
        let e = edges(
            Language::Python,
            "def run():\n    helper()\n\nclass C:\n    def go(self):\n        self.run()\n\nrun()\n",
        );
        assert_eq!(
            e,
            vec![
                edge("run", "helper", 2),
                edge("go", "run", 6),
                edge("", "run", 8)
            ]
        );
    }

    #[test]
    fn test_typescript_call_edges() {
        let e = edges(
            Language::TypeScript,
            "function outer() {\n  return inner(new Foo());\n}\nclass K {\n  method() { outer(); }\n}\n",
        );
        assert_eq!(
            e,
            vec![
                edge("outer", "Foo", 2),
                edge("outer", "inner", 2),
                edge("method", "outer", 5),
            ]
        );
    }

    #[test]
    fn test_java_call_edges() {
        let e = edges(
            Language::Java,
            "class C {\n  void run() {\n    process(x);\n  }\n  void process(int x) {}\n}",
        );
        assert_eq!(e, vec![edge("run", "process", 3)]);
    }

    #[test]
    fn test_go_call_edges() {
        let e = edges(
            Language::Go,
            "package p\nfunc alpha() {\n\tbeta()\n}\nfunc beta() {}\n",
        );
        assert_eq!(e, vec![edge("alpha", "beta", 3)]);
    }
}
