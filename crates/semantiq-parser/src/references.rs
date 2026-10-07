//! Extraction des références (occurrences d'identifiants) depuis l'AST.
//!
//! Contrairement à une recherche textuelle, seules les feuilles identifiants de
//! l'arbre sont retenues : les commentaires, chaînes et sous-chaînes d'autres
//! noms ne produisent jamais de référence. Le parcours est générique pour tous
//! les langages de code ; chaque occurrence est classée d'après son contexte
//! syntaxique (définition, import, appel, type ou simple référence).
//!
//! La résolution est purement nominale : deux symboles homonymes dans des
//! fichiers différents partagent leurs références.

use crate::language::Language;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tree_sitter::{Node, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    // Ordre = priorité croissante lors de la déduplication par ligne.
    Reference,
    Type,
    Call,
    Import,
    Definition,
}

impl ReferenceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReferenceKind::Reference => "reference",
            ReferenceKind::Type => "type",
            ReferenceKind::Call => "call",
            ReferenceKind::Import => "import",
            ReferenceKind::Definition => "definition",
        }
    }
}

impl std::str::FromStr for ReferenceKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "reference" => Ok(ReferenceKind::Reference),
            "type" => Ok(ReferenceKind::Type),
            "call" => Ok(ReferenceKind::Call),
            "import" => Ok(ReferenceKind::Import),
            "definition" => Ok(ReferenceKind::Definition),
            _ => Err(()),
        }
    }
}

/// Une occurrence d'identifiant. Dédupliquée par `(name, line)` : plusieurs
/// occurrences du même nom sur une ligne n'en font qu'une, avec le type le
/// plus prioritaire ; `count` est le nombre d'occurrences de ce type-là sur
/// la ligne (`f(f(x))` → call ×2 ; `retry(retry)` → call ×1, la référence
/// simple n'est pas comptée).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    pub name: String,
    /// Ligne 1-based.
    pub line: usize,
    pub kind: ReferenceKind,
    /// Occurrences de `kind` sur cette ligne (≥ 1).
    pub count: usize,
}

/// Noms plus courts ignorés : `i`, `x`, `_`… ne sont jamais recherchés et
/// gonfleraient l'index.
const MIN_NAME_LEN: usize = 2;

/// Profondeur maximale de remontée pour détecter un contexte d'import.
const MAX_IMPORT_DEPTH: usize = 8;

pub struct ReferenceExtractor;

impl ReferenceExtractor {
    pub fn extract(tree: &Tree, source: &str, language: Language) -> Vec<Reference> {
        if is_data_language(language) {
            return Vec::new();
        }

        let mut by_line: HashMap<(String, usize), (ReferenceKind, usize)> = HashMap::new();
        let mut cursor = tree.walk();
        let mut stack = vec![tree.root_node()];

        while let Some(node) = stack.pop() {
            if node.named_child_count() == 0 {
                if is_identifier(node)
                    && let Ok(text) = node.utf8_text(source.as_bytes())
                {
                    let line = node.start_position().row + 1;
                    let kind = classify(node);
                    for name in names_of(text) {
                        if name.chars().count() >= MIN_NAME_LEN {
                            by_line
                                .entry((name.to_string(), line))
                                .and_modify(|(k, n)| match kind.cmp(k) {
                                    std::cmp::Ordering::Greater => (*k, *n) = (kind, 1),
                                    std::cmp::Ordering::Equal => *n += 1,
                                    std::cmp::Ordering::Less => {}
                                })
                                .or_insert((kind, 1));
                        }
                    }
                }
                continue;
            }
            stack.extend(node.named_children(&mut cursor));
        }

        let mut refs: Vec<Reference> = by_line
            .into_iter()
            .map(|((name, line), (kind, count))| Reference {
                name,
                line,
                kind,
                count,
            })
            .collect();
        refs.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.name.cmp(&b.name)));
        refs
    }
}

fn is_data_language(language: Language) -> bool {
    matches!(
        language,
        Language::Html | Language::Json | Language::Yaml | Language::Toml
    )
}

fn is_identifier(node: Node) -> bool {
    let kind = node.kind();
    if kind.ends_with("identifier") {
        return true;
    }
    let parent = node.parent().map(|p| p.kind()).unwrap_or("");
    match kind {
        // Ruby (constantes / classes), PHP, Elixir (modules)
        "constant" | "name" | "alias" => true,
        // Bash : un `word` n'est un identifiant que comme nom de commande ou de fonction
        "word" => matches!(parent, "command_name" | "function_definition"),
        // Bash : variables (`$x`, `x=`)
        "variable_name" => true,
        _ => false,
    }
}

/// Un alias Elixir `A.Foo` est un seul nœud : on indexe aussi `Foo`.
fn names_of(text: &str) -> impl Iterator<Item = &str> {
    let last = text
        .rsplit_once('.')
        .map(|(_, last)| last)
        .filter(|last| !last.is_empty());
    std::iter::once(text).chain(last)
}

fn classify(node: Node) -> ReferenceKind {
    // Import d'abord : `import { Foo }` a un champ `name` dans un `*_specifier`.
    if in_import(node) {
        ReferenceKind::Import
    } else if is_definition_name(node) {
        ReferenceKind::Definition
    } else if is_callee(node) {
        ReferenceKind::Call
    } else if is_type(node) {
        ReferenceKind::Type
    } else {
        ReferenceKind::Reference
    }
}

fn is_field(parent: Node, child: Node, field: &str) -> bool {
    parent.child_by_field_name(field) == Some(child)
}

/// Le nœud est-il le nom déclaré par une définition (`fn foo`, `class Foo`…) ?
///
/// Le champ `name` existe aussi hors définitions (`Foo::new`, `obj.meth()` en
/// Java), d'où le filtre sur le type du parent.
fn is_definition_name(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let kind = parent.kind();

    // C/C++ : `int foo(...)` → function_declarator(declarator: identifier)
    if kind == "function_declarator" {
        return is_field(parent, node, "declarator");
    }
    // Bash : `foo() { ... }`
    if kind == "function_definition" && node.kind() == "word" {
        return true;
    }

    let declares = kind.contains("definition")
        || kind.contains("declaration")
        || kind.contains("declarator")
        || kind.ends_with("_item")
        || kind.ends_with("_spec")
        || kind.ends_with("_specifier")
        || matches!(kind, "class" | "module" | "method" | "singleton_method");
    declares && is_field(parent, node, "name")
}

fn in_import(node: Node) -> bool {
    let mut current = node.parent();
    for _ in 0..MAX_IMPORT_DEPTH {
        let Some(n) = current else {
            return false;
        };
        let kind = n.kind();
        if kind.contains("import")
            || matches!(
                kind,
                "use_declaration"
                    | "using_directive"
                    | "namespace_use_declaration"
                    | "preproc_include"
            )
        {
            return true;
        }
        current = n.parent();
    }
    false
}

/// Accès membre / chemin qualifié : `obj.meth`, `Foo::new`, `a.b.c`.
fn is_member_access(kind: &str) -> bool {
    matches!(
        kind,
        "field_expression"
            | "member_expression"
            | "scoped_identifier"
            | "attribute"
            | "selector_expression"
            | "navigation_expression"
            | "navigation_suffix"
            | "member_access_expression"
            | "qualified_identifier"
            | "qualified_name"
            | "dot"
    )
}

fn is_call(kind: &str) -> bool {
    matches!(
        kind,
        "call_expression"
            | "call"
            | "method_invocation"
            | "invocation_expression"
            | "function_call_expression"
            | "member_call_expression"
            | "scoped_call_expression"
            | "macro_invocation"
            | "new_expression"
            | "object_creation_expression"
            | "constructor_invocation"
            | "command_name"
    )
}

/// Le nœud est-il ce qui est appelé (et non un argument ou le receveur) ?
fn is_callee(node: Node) -> bool {
    // Remonte la partie droite des accès membres : dans `obj.meth()`, c'est
    // `meth` qui est appelé, pas `obj`.
    let mut current = node;
    let mut parent = node.parent();
    while let Some(p) = parent {
        if !is_member_access(p.kind()) {
            break;
        }
        let is_last = p.named_child(p.named_child_count().saturating_sub(1)) == Some(current);
        if !is_last {
            return false;
        }
        current = p;
        parent = p.parent();
    }

    let Some(call) = parent else {
        return false;
    };
    if !is_call(call.kind()) {
        return false;
    }

    const CALLEE_FIELDS: [&str; 7] = [
        "function",
        "name",
        "method",
        "constructor",
        "macro",
        "type",
        "target",
    ];
    if CALLEE_FIELDS.iter().any(|f| is_field(call, current, f)) {
        return true;
    }
    // Grammaires sans champ nommé (Kotlin, Scala, PHP `new`, Bash) : l'appelé
    // est le premier enfant, à condition que le receveur ne soit pas un champ.
    !["receiver", "object", "expression"]
        .iter()
        .any(|f| is_field(call, current, f))
        && call.named_child(0) == Some(current)
}

fn is_type(node: Node) -> bool {
    if node.kind() == "type_identifier" {
        return true;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    let kind = parent.kind();
    kind.contains("type")
        || matches!(
            kind,
            "superclass" | "base_class_clause" | "extends_clause" | "base_list" | "base_clause"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LanguageSupport;

    fn refs(language: Language, source: &str) -> Vec<Reference> {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(language, source).unwrap();
        ReferenceExtractor::extract(&tree, source, language)
    }

    fn kinds_of(refs: &[Reference], name: &str) -> Vec<(usize, ReferenceKind)> {
        refs.iter()
            .filter(|r| r.name == name)
            .map(|r| (r.line, r.kind))
            .collect()
    }

    #[test]
    fn test_ignores_comments_and_strings() {
        let r = refs(
            Language::Rust,
            "fn helper() {}\n// helper in a comment\nfn main() { let s = \"helper\"; helper(); }",
        );
        assert_eq!(
            kinds_of(&r, "helper"),
            vec![(1, ReferenceKind::Definition), (3, ReferenceKind::Call)]
        );
    }

    #[test]
    fn test_ignores_substrings() {
        let r = refs(Language::Python, "def run(): pass\nrunner = 1\nrun()\n");
        assert_eq!(
            kinds_of(&r, "run"),
            vec![(1, ReferenceKind::Definition), (3, ReferenceKind::Call)]
        );
    }

    #[test]
    fn test_rust_kinds() {
        let r = refs(
            Language::Rust,
            "use crate::a::Foo;\nfn f(x: Foo) -> Foo {\n    let y = obj.meth(x);\n    Foo::new()\n}",
        );
        assert_eq!(kinds_of(&r, "Foo")[0], (1, ReferenceKind::Import));
        assert_eq!(kinds_of(&r, "Foo")[1], (2, ReferenceKind::Type));
        assert_eq!(kinds_of(&r, "meth"), vec![(3, ReferenceKind::Call)]);
        assert_eq!(kinds_of(&r, "obj"), vec![(3, ReferenceKind::Reference)]);
        assert_eq!(kinds_of(&r, "new"), vec![(4, ReferenceKind::Call)]);
    }

    #[test]
    fn test_typescript_kinds() {
        let r = refs(
            Language::TypeScript,
            "import { Foo } from './a';\nfunction build(x: Bar): Foo {\n  return new Foo(helper(x));\n}",
        );
        assert_eq!(
            kinds_of(&r, "Foo"),
            vec![
                (1, ReferenceKind::Import),
                (2, ReferenceKind::Type),
                (3, ReferenceKind::Call)
            ]
        );
        assert_eq!(kinds_of(&r, "build"), vec![(2, ReferenceKind::Definition)]);
        assert_eq!(kinds_of(&r, "helper"), vec![(3, ReferenceKind::Call)]);
    }

    #[test]
    fn test_java_method_invocation() {
        let r = refs(
            Language::Java,
            "class C {\n  void run() {\n    obj.process(x);\n    process(y);\n  }\n}",
        );
        assert_eq!(kinds_of(&r, "run"), vec![(2, ReferenceKind::Definition)]);
        assert_eq!(
            kinds_of(&r, "process"),
            vec![(3, ReferenceKind::Call), (4, ReferenceKind::Call)]
        );
        assert_eq!(kinds_of(&r, "obj"), vec![(3, ReferenceKind::Reference)]);
    }

    #[test]
    fn test_c_function_definition() {
        let r = refs(Language::C, "int compute(int a) { return helper(a); }");
        assert_eq!(
            kinds_of(&r, "compute"),
            vec![(1, ReferenceKind::Definition)]
        );
        assert_eq!(kinds_of(&r, "helper"), vec![(1, ReferenceKind::Call)]);
    }

    #[test]
    fn test_elixir_alias_last_segment() {
        let r = refs(
            Language::Elixir,
            "defmodule M do\n  def f, do: A.Foo.new()\nend",
        );
        assert!(r.iter().any(|x| x.name == "Foo" && x.line == 2));
    }

    #[test]
    fn test_same_line_keeps_highest_priority() {
        let r = refs(Language::Rust, "fn rec() { rec() }");
        assert_eq!(kinds_of(&r, "rec"), vec![(1, ReferenceKind::Definition)]);
        // The call folded into the definition row is not counted.
        assert_eq!(r.iter().find(|x| x.name == "rec").unwrap().count, 1);
    }

    #[test]
    fn test_same_line_counts_occurrences() {
        let r = refs(
            Language::Rust,
            "fn run() {\n    wrap(wrap(1));\n    once();\n}",
        );
        let count = |name: &str| r.iter().find(|x| x.name == name).unwrap().count;
        assert_eq!(count("wrap"), 2);
        assert_eq!(count("once"), 1);
    }

    #[test]
    fn test_same_line_counts_only_the_kept_kind() {
        // One call plus one plain reference of the same name: one call.
        let r = refs(
            Language::Rust,
            "fn run(retry: fn(u32)) { retry(retry as u32) }",
        );
        let retry = r.iter().find(|x| x.name == "retry" && x.line == 1).unwrap();
        assert_eq!(retry.kind, ReferenceKind::Call);
        assert_eq!(retry.count, 1);
    }

    #[test]
    fn test_data_languages_have_no_references() {
        assert!(refs(Language::Json, "{\"key\": \"value\"}").is_empty());
    }

    #[test]
    fn test_short_names_skipped() {
        let r = refs(Language::Python, "x = 1\nprint(x)\n");
        assert!(r.iter().all(|r| r.name != "x"));
    }
}
