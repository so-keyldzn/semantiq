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

        let mut record = |name: &str, line: usize, kind: ReferenceKind| {
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
        };

        while let Some(node) = stack.pop() {
            if node.named_child_count() == 0 {
                let line = node.start_position().row + 1;
                if is_identifier(node)
                    && let Ok(text) = node.utf8_text(source.as_bytes())
                    && let Some(kind) = classify(node, text, source.as_bytes(), language)
                {
                    for name in names_of(text) {
                        record(name, line, kind);
                    }
                } else if let Some(name) = attribute_string_name(node, source, language) {
                    record(name, line, ReferenceKind::Reference);
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

/// Profondeur maximale entre une chaîne et l'attribut qui la contient.
const MAX_ATTRIBUTE_DEPTH: usize = 6;

/// Nœud d'attribut / décorateur / annotation dont les arguments peuvent nommer
/// un symbole dans une chaîne : `#[serde(default = "f")]`, `@fixture(name="db")`,
/// `@DependsOn("bean")`. Les autres langages gardent la règle « une chaîne ne
/// référence jamais ».
fn attribute_kind(language: Language) -> Option<&'static str> {
    match language {
        Language::Rust => Some("attribute"),
        Language::Python => Some("decorator"),
        Language::Java | Language::Kotlin => Some("annotation"),
        Language::CSharp => Some("attribute"),
        _ => None,
    }
}

/// Le contenu d'une chaîne littérale située dans les arguments d'un attribut,
/// s'il a la forme d'un identifiant ou d'un chemin (`default_true`,
/// `Default::default`, `my_mod::serialize`) : la fonction qu'il nomme est
/// utilisée par la macro ou le framework, donc référencée. Retourne le dernier
/// segment du chemin. `#[doc = "..."]` est un commentaire, pas un argument.
fn attribute_string_name<'a>(node: Node, source: &'a str, language: Language) -> Option<&'a str> {
    // Rust / Python / Kotlin `string_content`, Java `string_fragment`,
    // C# (tree-sitter-c-sharp 0.23) `string_literal_content`.
    if !matches!(
        node.kind(),
        "string_content" | "string_fragment" | "string_literal_content"
    ) {
        return None;
    }
    let wanted = attribute_kind(language)?;
    let mut current = node.parent();
    let mut found = false;
    for _ in 0..MAX_ATTRIBUTE_DEPTH {
        let Some(n) = current else {
            break;
        };
        if n.kind() == wanted {
            // Rust : `#[doc = "..."]` porte un champ `value`, pas `arguments`.
            if language == Language::Rust && n.child_by_field_name("arguments").is_none() {
                return None;
            }
            found = true;
            break;
        }
        current = n.parent();
    }
    if !found {
        return None;
    }
    let text = node.utf8_text(source.as_bytes()).ok()?;
    let is_ident = |s: &str| {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
            && chars.all(|c| c.is_alphanumeric() || c == '_')
    };
    let mut segments = text.split("::");
    let mut last = segments.next()?;
    if !is_ident(last) {
        return None;
    }
    for segment in segments {
        if !is_ident(segment) {
            return None;
        }
        last = segment;
    }
    Some(last)
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

/// Classe une occurrence ; `None` = mot-clé à ne pas indexer (Elixir `def`).
fn classify(node: Node, text: &str, source: &[u8], language: Language) -> Option<ReferenceKind> {
    if language == Language::Elixir
        && let Some(kind) = classify_elixir(node, text, source)
    {
        return kind;
    }
    // Import d'abord : `import { Foo }` a un champ `name` dans un `*_specifier`.
    Some(if in_import(node) {
        ReferenceKind::Import
    } else if is_definition_name(node) {
        ReferenceKind::Definition
    } else if is_callee(node) {
        ReferenceKind::Call
    } else if is_type(node) {
        ReferenceKind::Type
    } else {
        ReferenceKind::Reference
    })
}

/// Mots-clés Elixir qui déclarent un symbole : `def foo(x)` est un `call` dont
/// le `target` est `def` et dont le premier argument porte le nom déclaré.
/// Même liste que `queries/elixir/tags.scm`, plus `defimpl`.
const ELIXIR_DEFINERS: [&str; 13] = [
    "defmodule",
    "defprotocol",
    "defimpl",
    "def",
    "defp",
    "defmacro",
    "defmacrop",
    "defguard",
    "defguardp",
    "defdelegate",
    "defn",
    "defnp",
    "defstruct",
];

/// Directives Elixir : leurs arguments sont des imports, pas des appels.
const ELIXIR_DIRECTIVES: [&str; 4] = ["alias", "import", "use", "require"];

/// Elixir : les mots-clés `def*` / `alias` / `import`… sont des `call` dans
/// l'AST. Retourne `Some(None)` pour le mot-clé lui-même (pas indexé),
/// `Some(Some(Definition))` pour le nom qu'il déclare, `Some(Some(Import))`
/// pour l'argument d'une directive, et `None` quand le classement générique
/// s'applique.
fn classify_elixir(node: Node, text: &str, source: &[u8]) -> Option<Option<ReferenceKind>> {
    let parent = node.parent()?;
    // Le mot-clé : `def` dans `(call target: (identifier))`.
    if node.kind() == "identifier"
        && parent.kind() == "call"
        && is_field(parent, node, "target")
        && (ELIXIR_DEFINERS.contains(&text) || ELIXIR_DIRECTIVES.contains(&text))
    {
        return Some(None);
    }
    // Le nom déclaré : premier argument du mot-clé, directement (`def foo`,
    // `defmodule M`), comme cible d'un appel (`def foo(x)`) ou à gauche d'un
    // `when` (`def foo(x) when ...`).
    let head = if parent.kind() == "arguments" {
        node
    } else if parent.kind() == "call" && is_field(parent, node, "target") {
        let mut head = parent;
        if let Some(op) = head.parent()
            && op.kind() == "binary_operator"
            && is_field(op, head, "left")
        {
            head = op;
        }
        head
    } else {
        return None;
    };
    let arguments = head.parent()?;
    if arguments.kind() != "arguments" || arguments.named_child(0) != Some(head) {
        return None;
    }
    let keyword = arguments.parent()?;
    if keyword.kind() != "call" {
        return None;
    }
    let keyword_text = keyword
        .child_by_field_name("target")
        .and_then(|t| t.utf8_text(source).ok())?;
    // `defimpl Proto, for: T` ne déclare ni `Proto` ni `T`.
    if ELIXIR_DEFINERS.contains(&keyword_text) && keyword_text != "defimpl" {
        Some(Some(ReferenceKind::Definition))
    } else if ELIXIR_DIRECTIVES.contains(&keyword_text) {
        Some(Some(ReferenceKind::Import))
    } else {
        None
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
    // Rust : les arguments d'une macro (`assert_eq!(f(x), 1)`, `vec![g()]`,
    // `tracing::info!(n = h())`) ne sont pas parsés en expressions.
    if call.kind() == "token_tree" {
        return is_macro_argument_call(current);
    }
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

/// Rust : un identifiant dans le `token_tree` d'une `macro_invocation` est un
/// appel quand il est suivi de `(` (`f(x)`) ou de `!(` (macro imbriquée).
/// Volontairement conservateur : `x = f()` donne `f` seul, `Foo::bar()` donne
/// `bar` seul. Les `token_tree` d'attributs (`#[cfg(all(...))]`) et de
/// `macro_rules!` ne sont pas concernés.
fn is_macro_argument_call(node: Node) -> bool {
    let mut outer = node.parent();
    while let Some(tree) = outer {
        if tree.kind() != "token_tree" {
            break;
        }
        outer = tree.parent();
    }
    if outer.map(|n| n.kind()) != Some("macro_invocation") {
        return false;
    }
    let mut next = node.next_sibling();
    if next.is_some_and(|n| n.kind() == "!") {
        next = next.and_then(|n| n.next_sibling());
    }
    next.is_some_and(|n| n.kind() == "token_tree" && n.child(0).is_some_and(|c| c.kind() == "("))
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
    fn test_elixir_definitions_are_not_calls() {
        let r = refs(
            Language::Elixir,
            "defmodule Mod do\n  def foo(x) do\n    bar(x)\n  end\n  defp bar(x), do: x\n  def baz(x) when is_integer(x), do: x\n  def noparen, do: :ok\n  alias A.B\n  import Enum\nend\n",
        );
        assert_eq!(kinds_of(&r, "Mod"), vec![(1, ReferenceKind::Definition)]);
        assert_eq!(kinds_of(&r, "foo"), vec![(2, ReferenceKind::Definition)]);
        assert_eq!(
            kinds_of(&r, "bar"),
            vec![(3, ReferenceKind::Call), (5, ReferenceKind::Definition)]
        );
        assert_eq!(kinds_of(&r, "baz"), vec![(6, ReferenceKind::Definition)]);
        assert_eq!(kinds_of(&r, "is_integer"), vec![(6, ReferenceKind::Call)]);
        assert_eq!(
            kinds_of(&r, "noparen"),
            vec![(7, ReferenceKind::Definition)]
        );
        assert_eq!(kinds_of(&r, "A.B"), vec![(8, ReferenceKind::Import)]);
        assert_eq!(kinds_of(&r, "Enum"), vec![(9, ReferenceKind::Import)]);
        for keyword in ["defmodule", "def", "defp", "alias", "import"] {
            assert!(kinds_of(&r, keyword).is_empty(), "{keyword} indexed");
        }
    }

    #[test]
    fn test_rust_calls_inside_macro_arguments() {
        let r = refs(
            Language::Rust,
            "fn t() {\n    assert_eq!(compute(1), 1);\n    println!(\"{}\", compute(2));\n    let v = vec![make(), obj.meth(), Ty::build()];\n    tracing::info!(count = total(), \"m\");\n    outer!(inner!(deep()));\n    debug!(flag);\n}",
        );
        assert_eq!(
            kinds_of(&r, "compute"),
            vec![(2, ReferenceKind::Call), (3, ReferenceKind::Call)]
        );
        assert_eq!(kinds_of(&r, "make"), vec![(4, ReferenceKind::Call)]);
        assert_eq!(kinds_of(&r, "meth"), vec![(4, ReferenceKind::Call)]);
        assert_eq!(kinds_of(&r, "obj"), vec![(4, ReferenceKind::Reference)]);
        assert_eq!(kinds_of(&r, "build"), vec![(4, ReferenceKind::Call)]);
        assert_eq!(kinds_of(&r, "Ty"), vec![(4, ReferenceKind::Reference)]);
        assert_eq!(kinds_of(&r, "total"), vec![(5, ReferenceKind::Call)]);
        assert_eq!(kinds_of(&r, "count"), vec![(5, ReferenceKind::Reference)]);
        assert_eq!(kinds_of(&r, "inner"), vec![(6, ReferenceKind::Call)]);
        assert_eq!(kinds_of(&r, "deep"), vec![(6, ReferenceKind::Call)]);
        assert_eq!(kinds_of(&r, "flag"), vec![(7, ReferenceKind::Reference)]);
    }

    #[test]
    fn test_rust_attribute_and_macro_rules_token_trees_are_not_calls() {
        let r = refs(
            Language::Rust,
            "#[cfg(all(unix, feature = \"x\"))]\nfn t() {}\nmacro_rules! m { ($e:expr) => { wrap($e) }; }",
        );
        assert_eq!(kinds_of(&r, "all"), vec![(1, ReferenceKind::Reference)]);
        assert_eq!(kinds_of(&r, "wrap"), vec![(3, ReferenceKind::Reference)]);
    }

    #[test]
    fn test_rust_attribute_strings_reference_functions() {
        let r = refs(
            Language::Rust,
            "fn default_true() -> bool { true }\n#[derive(serde::Deserialize)]\npub struct Cfg {\n    #[serde(default = \"default_true\", skip_serializing_if = \"is_false\")]\n    pub on: bool,\n    #[serde(with = \"my_mod::as_str\", rename = \"not an identifier\")]\n    pub w: u8,\n}\n#[doc = \"hidden\"]\nfn g() { let s = \"default_true\"; }\n",
        );
        assert_eq!(
            kinds_of(&r, "default_true"),
            vec![
                (1, ReferenceKind::Definition),
                (4, ReferenceKind::Reference)
            ]
        );
        assert_eq!(
            kinds_of(&r, "is_false"),
            vec![(4, ReferenceKind::Reference)]
        );
        assert_eq!(kinds_of(&r, "as_str"), vec![(6, ReferenceKind::Reference)]);
        assert!(kinds_of(&r, "my_mod::as_str").is_empty());
        assert!(kinds_of(&r, "not an identifier").is_empty());
        assert!(kinds_of(&r, "hidden").is_empty());
    }

    #[test]
    fn test_decorator_and_annotation_strings_reference_names() {
        let py = refs(
            Language::Python,
            "@pytest.fixture(name=\"session_db\")\ndef db(): pass\n@app.route(\"/users\")\ndef users(): return \"session_db\"\n",
        );
        assert_eq!(
            kinds_of(&py, "session_db"),
            vec![(1, ReferenceKind::Reference)]
        );
        assert!(kinds_of(&py, "/users").is_empty());

        let java = refs(
            Language::Java,
            "class C {\n  @DependsOn(\"initCache\")\n  void run() { String s = \"initCache\"; }\n}",
        );
        assert_eq!(
            kinds_of(&java, "initCache"),
            vec![(2, ReferenceKind::Reference)]
        );

        let csharp = refs(
            Language::CSharp,
            "class C {\n  [DependsOn(\"initCache\")]\n  void Run() { var s = \"initCache\"; }\n}",
        );
        assert_eq!(
            kinds_of(&csharp, "initCache"),
            vec![(2, ReferenceKind::Reference)]
        );

        let kotlin = refs(
            Language::Kotlin,
            "class C {\n  @Named(\"initCache\")\n  fun run() { val s = \"initCache\" }\n}",
        );
        assert_eq!(
            kinds_of(&kotlin, "initCache"),
            vec![(2, ReferenceKind::Reference)]
        );
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
