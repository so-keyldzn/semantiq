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
//!
//! Chemin chaud (un tiers du temps d'indexation, #158) : les prédicats sur
//! les types de nœuds (`kind.contains("definition")`, `kind ==
//! "call_expression"`…) sont évalués une fois par grammaire sur chaque nom de
//! kind et rangés dans une [`KindTable`] (un mot de drapeaux par id de kind,
//! plus les ids des champs consultés), et le contexte d'une feuille est lu
//! dans la pile d'ancêtres du parcours plutôt que par `Node::parent()`, que
//! tree-sitter résout en redescendant depuis la racine à chaque appel.

use crate::language::{Grammar, Language};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
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
    pub fn extract<'s>(tree: &Tree, source: &'s str, language: Language) -> Vec<Reference> {
        if is_data_language(language) {
            return Vec::new();
        }
        let table = kind_table(tree, language);
        let k = &*table;

        // Clés empruntées à `source` : une seule allocation par référence
        // produite, à la fin, et non par occurrence rencontrée.
        let mut by_line: HashMap<(&str, usize), (ReferenceKind, usize)> = HashMap::new();
        let mut cursor = tree.walk();
        // Parcours en profondeur ; `ancestors` est le chemin de la racine au
        // parent du nœud visité (profondeur = nombre d'ancêtres).
        let mut stack: Vec<(Node, usize)> = vec![(tree.root_node(), 0)];
        let mut ancestors: Vec<Node> = Vec::new();

        let mut record = |name: &'s str, line: usize, kind: ReferenceKind| {
            if name.chars().count() >= MIN_NAME_LEN {
                by_line
                    .entry((name, line))
                    .and_modify(|(k, n)| match kind.cmp(k) {
                        std::cmp::Ordering::Greater => (*k, *n) = (kind, 1),
                        std::cmp::Ordering::Equal => *n += 1,
                        std::cmp::Ordering::Less => {}
                    })
                    .or_insert((kind, 1));
            }
        };

        while let Some((node, depth)) = stack.pop() {
            ancestors.truncate(depth);
            if node.named_child_count() == 0 {
                let line = node.start_position().row + 1;
                let leaf = Leaf {
                    node,
                    ancestors: &ancestors,
                    k,
                };
                if leaf.is_identifier()
                    && let Ok(text) = node.utf8_text(source.as_bytes())
                    && let Some(kind) = leaf.classify(text, source.as_bytes(), language)
                {
                    for name in names_of(text) {
                        record(name, line, kind);
                    }
                } else if let Some(name) = leaf.attribute_string_name(source, language) {
                    record(name, line, ReferenceKind::Reference);
                }
                continue;
            }
            ancestors.push(node);
            stack.extend(node.named_children(&mut cursor).map(|c| (c, depth + 1)));
        }

        let mut refs: Vec<Reference> = by_line
            .into_iter()
            .map(|((name, line), (kind, count))| Reference {
                name: name.to_string(),
                line,
                kind,
                count,
            })
            .collect();
        refs.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.name.cmp(&b.name)));
        refs
    }
}

/// Drapeaux de [`KindTable::flags`] : un bit par prédicat sur le nom d'un
/// kind, calculé par [`kind_flags`].
mod flag {
    /// `is_identifier` : suffixe `identifier`, `constant` / `name` / `alias`
    /// (Ruby, PHP, Elixir), `variable_name` (Bash).
    pub const IDENTIFIER_LIKE: u64 = 1 << 0;
    /// Bash `word`.
    pub const WORD: u64 = 1 << 1;
    /// Parents qui font d'un `word` un identifiant.
    pub const WORD_CONTEXT: u64 = 1 << 2;
    /// Contenu d'une chaîne littérale.
    pub const STRING_CONTENT: u64 = 1 << 3;
    /// Attribut / décorateur / annotation du langage ([`super::attribute_kind`]).
    pub const ATTRIBUTE: u64 = 1 << 4;
    pub const EQ: u64 = 1 << 5;
    pub const TOKEN_TREE: u64 = 1 << 6;
    pub const IDENTIFIER: u64 = 1 << 7;
    /// Le kind `call` (Elixir, Ruby).
    pub const CALL_NODE: u64 = 1 << 8;
    pub const ARGUMENTS: u64 = 1 << 9;
    pub const BINARY_OPERATOR: u64 = 1 << 10;
    pub const MEMBER_EXPRESSION: u64 = 1 << 11;
    /// Élément JSX ouvrant, fermant ou auto-fermant.
    pub const JSX_ELEMENT: u64 = 1 << 12;
    pub const JSX_CLOSING_ELEMENT: u64 = 1 << 13;
    pub const JSX_NAMESPACE_NAME: u64 = 1 << 14;
    pub const JSX_ATTRIBUTE: u64 = 1 << 15;
    pub const EXPORT_SPECIFIER: u64 = 1 << 16;
    pub const FUNCTION_DECLARATOR: u64 = 1 << 17;
    pub const FUNCTION_DEFINITION: u64 = 1 << 18;
    /// Nœud dont le champ `name` est un nom déclaré.
    pub const DECLARES: u64 = 1 << 19;
    /// Nœud d'import (`import_*`, `use_declaration`…).
    pub const IMPORT_CONTEXT: u64 = 1 << 20;
    pub const EXPORT_STATEMENT: u64 = 1 << 21;
    /// Accès membre / chemin qualifié ([`super::is_member_access`]).
    pub const MEMBER_ACCESS: u64 = 1 << 22;
    /// Nœud d'appel ([`super::is_call`]).
    pub const CALL: u64 = 1 << 23;
    pub const MACRO_INVOCATION: u64 = 1 << 24;
    pub const BANG: u64 = 1 << 25;
    pub const LPAREN: u64 = 1 << 26;
    pub const TYPE_IDENTIFIER: u64 = 1 << 27;
    /// Parent qui fait d'un identifiant un type ([`super::is_type`]).
    pub const TYPE_CONTEXT: u64 = 1 << 28;
}

/// Drapeaux d'un nom de kind ; `attribute` est le kind d'attribut du langage.
fn kind_flags(kind: &str, attribute: Option<&str>) -> u64 {
    let mut f = 0;
    let mut set = |on: bool, bit: u64| {
        if on {
            f |= bit;
        }
    };
    set(
        kind.ends_with("identifier")
            || matches!(kind, "constant" | "name" | "alias" | "variable_name"),
        flag::IDENTIFIER_LIKE,
    );
    set(kind == "word", flag::WORD);
    set(
        matches!(kind, "command_name" | "function_definition"),
        flag::WORD_CONTEXT,
    );
    // Rust / Python / Kotlin `string_content`, Java `string_fragment`,
    // C# (tree-sitter-c-sharp 0.23) `string_literal_content`.
    set(
        matches!(
            kind,
            "string_content" | "string_fragment" | "string_literal_content"
        ),
        flag::STRING_CONTENT,
    );
    set(Some(kind) == attribute, flag::ATTRIBUTE);
    set(kind == "=", flag::EQ);
    set(kind == "token_tree", flag::TOKEN_TREE);
    set(kind == "identifier", flag::IDENTIFIER);
    set(kind == "call", flag::CALL_NODE);
    set(kind == "arguments", flag::ARGUMENTS);
    set(kind == "binary_operator", flag::BINARY_OPERATOR);
    set(kind == "member_expression", flag::MEMBER_EXPRESSION);
    set(
        matches!(
            kind,
            "jsx_opening_element" | "jsx_closing_element" | "jsx_self_closing_element"
        ),
        flag::JSX_ELEMENT,
    );
    set(kind == "jsx_closing_element", flag::JSX_CLOSING_ELEMENT);
    set(kind == "jsx_namespace_name", flag::JSX_NAMESPACE_NAME);
    set(kind == "jsx_attribute", flag::JSX_ATTRIBUTE);
    set(kind == "export_specifier", flag::EXPORT_SPECIFIER);
    set(kind == "function_declarator", flag::FUNCTION_DECLARATOR);
    set(kind == "function_definition", flag::FUNCTION_DEFINITION);
    set(
        kind.contains("definition")
            || kind.contains("declaration")
            || kind.contains("declarator")
            || kind.ends_with("_item")
            || kind.ends_with("_spec")
            || kind.ends_with("_specifier")
            || matches!(kind, "class" | "module" | "method" | "singleton_method"),
        flag::DECLARES,
    );
    set(
        kind.contains("import")
            || matches!(
                kind,
                "use_declaration"
                    | "using_directive"
                    | "namespace_use_declaration"
                    | "preproc_include"
            ),
        flag::IMPORT_CONTEXT,
    );
    set(kind == "export_statement", flag::EXPORT_STATEMENT);
    set(is_member_access(kind), flag::MEMBER_ACCESS);
    set(is_call(kind), flag::CALL);
    set(kind == "macro_invocation", flag::MACRO_INVOCATION);
    set(kind == "!", flag::BANG);
    set(kind == "(", flag::LPAREN);
    set(kind == "type_identifier", flag::TYPE_IDENTIFIER);
    set(
        kind.contains("type")
            || matches!(
                kind,
                "superclass" | "base_class_clause" | "extends_clause" | "base_list" | "base_clause"
            ),
        flag::TYPE_CONTEXT,
    );
    f
}

/// Prédicats du classement précalculés pour une grammaire : un mot de
/// drapeaux par id de kind et les ids des champs consultés (0 = la grammaire
/// n'a pas ce champ, `child_by_field_id(0)` ne trouve rien).
struct KindTable {
    flags: Vec<u64>,
    /// Le langage a un kind d'attribut ([`attribute_kind`]).
    has_attribute: bool,
    name: u16,
    declarator: u16,
    source: u16,
    target: u16,
    left: u16,
    property: u16,
    /// Champs de l'appelé d'un nœud d'appel.
    callee: [u16; 7],
    /// Champs du receveur d'un appel (jamais l'appelé).
    receiver: [u16; 3],
}

impl KindTable {
    fn build(grammar: &tree_sitter::Language, language: Language) -> Self {
        let attribute = attribute_kind(language);
        let flags = (0..grammar.node_kind_count())
            .map(|id| {
                grammar
                    .node_kind_for_id(id as u16)
                    .map_or(0, |kind| kind_flags(kind, attribute))
            })
            .collect();
        let field = |name: &str| grammar.field_id_for_name(name).map_or(0, |id| id.get());
        KindTable {
            flags,
            has_attribute: attribute.is_some(),
            name: field("name"),
            declarator: field("declarator"),
            source: field("source"),
            target: field("target"),
            left: field("left"),
            property: field("property"),
            callee: [
                field("function"),
                field("name"),
                field("method"),
                field("constructor"),
                field("macro"),
                field("type"),
                field("target"),
            ],
            receiver: [field("receiver"), field("object"), field("expression")],
        }
    }

    /// Le kind de `node` a-t-il le drapeau ? (`ERROR` et les ids hors table
    /// n'en ont aucun.)
    #[inline]
    fn has(&self, node: Node, flag: u64) -> bool {
        self.flags
            .get(node.kind_id() as usize)
            .is_some_and(|f| f & flag != 0)
    }

    /// `child` est-il le champ `field` de `parent` ?
    #[inline]
    fn is_field(&self, parent: Node, child: Node, field: u16) -> bool {
        parent.child_by_field_id(field) == Some(child)
    }
}

static KIND_TABLES: OnceLock<Mutex<HashMap<Grammar, Arc<KindTable>>>> = OnceLock::new();

/// La table de la grammaire qui a produit `tree`, construite au premier
/// fichier de chaque grammaire puis partagée. Un arbre produit par une autre
/// grammaire que celles de `language` (appel direct hors indexeur) reçoit une
/// table construite sur place, sans cache.
fn kind_table(tree: &Tree, language: Language) -> Arc<KindTable> {
    let grammar = [Grammar::Language(language), Grammar::Tsx]
        .into_iter()
        .find(|g| g.parsed(tree));
    let Some(grammar) = grammar else {
        return Arc::new(KindTable::build(&tree.language(), language));
    };
    let mut tables = KIND_TABLES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    Arc::clone(
        tables.entry(grammar).or_insert_with(|| {
            Arc::new(KindTable::build(&grammar.tree_sitter_language(), language))
        }),
    )
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

/// Rust : clés d'attribut dont la valeur est un chemin de fonction
/// (`#[serde(default = "f")]`, `deserialize_with`, `validate(custom = "g")`,
/// `schemars(schema_with = "h")`, `getset(getter = "i")`). Toute autre clé
/// (`rename`, `feature`, `target_os`, `tag`…) nomme une chaîne, pas un symbole.
const RUST_FUNCTION_PATH_KEYS: [&str; 9] = [
    "default",
    "with",
    "serialize_with",
    "deserialize_with",
    "skip_serializing_if",
    "custom",
    "function",
    "schema_with",
    "getter",
];

/// Décorateurs / annotations dont les chaînes ne nomment jamais un symbole du
/// projet : noms de colonnes, de propriétés JSON, de routes, d'avertissements.
const NON_SYMBOL_ATTRIBUTES: [&str; 26] = [
    "Suppress",
    "SuppressWarnings",
    "SuppressMessage",
    "JsonProperty",
    "JsonPropertyName",
    "JsonAlias",
    "SerializedName",
    "Column",
    "JoinColumn",
    "Table",
    "Entity",
    "RequestMapping",
    "GetMapping",
    "PostMapping",
    "PutMapping",
    "PatchMapping",
    "DeleteMapping",
    "RequestParam",
    "PathVariable",
    "Route",
    "HttpGet",
    "HttpPost",
    "Obsolete",
    "Display",
    "DisplayName",
    "parametrize",
];

/// Nom nu d'un attribut : `@pytest.mark.parametrize(...)` → `parametrize`,
/// `[JsonProperty("x")]` → `JsonProperty`, `@Suppress("X")` → `Suppress`.
fn attribute_name<'a>(attribute: Node, source: &'a str) -> Option<&'a str> {
    let text = attribute.utf8_text(source.as_bytes()).ok()?;
    let head = text.split('(').next()?.trim();
    head.rsplit(['.', '@', '[', ' ']).next().map(str::trim)
}

/// Les frères de `child` parmi les enfants (anonymes compris) de `parent` :
/// `(précédent, suivant)`. Lit la liste des enfants une fois au lieu de
/// `prev_sibling()` / `next_sibling()`, qui repartent de la racine.
fn siblings<'t>(parent: Node<'t>, child: Node<'t>) -> (Option<Node<'t>>, Option<Node<'t>>) {
    let mut cursor = parent.walk();
    let mut previous = None;
    let mut children = parent.children(&mut cursor);
    for c in children.by_ref() {
        if c == child {
            return (previous, children.next());
        }
        previous = Some(c);
    }
    (None, None)
}

fn is_data_language(language: Language) -> bool {
    matches!(
        language,
        Language::Html | Language::Json | Language::Yaml | Language::Toml
    )
}

/// Un alias Elixir `A.Foo` est un seul nœud : on indexe aussi `Foo`.
fn names_of(text: &str) -> impl Iterator<Item = &str> {
    let last = text
        .rsplit_once('.')
        .map(|(_, last)| last)
        .filter(|last| !last.is_empty());
    std::iter::once(text).chain(last)
}

/// Une feuille de l'arbre avec son chemin : `ancestors[0]` est la racine, le
/// dernier élément le parent. Toute la classification remonte par `up(n)`.
#[derive(Clone, Copy)]
struct Leaf<'a, 't> {
    node: Node<'t>,
    ancestors: &'a [Node<'t>],
    k: &'a KindTable,
}

impl<'t> Leaf<'_, 't> {
    /// Le `n`-ième ancêtre : `up(0)` est le parent.
    #[inline]
    fn up(&self, n: usize) -> Option<Node<'t>> {
        self.ancestors
            .len()
            .checked_sub(n + 1)
            .map(|i| self.ancestors[i])
    }

    /// Le nœud de profondeur `n` au-dessus de la feuille (`node_at(0)` est
    /// la feuille elle-même, `node_at(1)` son parent).
    #[inline]
    fn node_at(&self, n: usize) -> Option<Node<'t>> {
        if n == 0 {
            Some(self.node)
        } else {
            self.up(n - 1)
        }
    }

    /// Feuille identifiant : suffixe `identifier`, `constant` / `name` /
    /// `alias` (Ruby, PHP, Elixir), `variable_name` (Bash `$x`, `x=`) ; un
    /// `word` Bash seulement comme nom de commande ou de fonction.
    fn is_identifier(&self) -> bool {
        let k = self.k;
        if k.has(self.node, flag::IDENTIFIER_LIKE) {
            return true;
        }
        k.has(self.node, flag::WORD) && self.up(0).is_some_and(|p| k.has(p, flag::WORD_CONTEXT))
    }

    /// Rust : la chaîne est-elle la valeur d'une clé `key = "..."` de
    /// [`RUST_FUNCTION_PATH_KEYS`] ? Dans le `token_tree`, les frères
    /// précédents du `string_literal` (parent de la feuille) sont `=` puis le
    /// jeton de la clé (un `identifier`, ou un mot-clé comme `default` dont
    /// le kind est le texte lui-même).
    fn is_rust_function_path_value(&self, source: &str) -> bool {
        let k = self.k;
        let (Some(string_literal), Some(token_tree)) = (self.up(0), self.up(1)) else {
            return false;
        };
        let (Some(eq), _) = siblings(token_tree, string_literal) else {
            return false;
        };
        if !k.has(eq, flag::EQ) {
            return false;
        }
        siblings(token_tree, eq).0.is_some_and(|key| {
            !k.has(key, flag::TOKEN_TREE)
                && key
                    .utf8_text(source.as_bytes())
                    .is_ok_and(|k| RUST_FUNCTION_PATH_KEYS.contains(&k))
        })
    }

    /// Le contenu d'une chaîne littérale située dans les arguments d'un
    /// attribut, s'il a la forme d'un identifiant ou d'un chemin
    /// (`default_true`, `Default::default`, `my_mod::serialize`) : la
    /// fonction qu'il nomme est utilisée par la macro ou le framework, donc
    /// référencée. Retourne le dernier segment du chemin.
    ///
    /// Périmètre volontairement étroit, pour ne pas inventer d'usages dans
    /// `find_refs` / `impact` / repo map : en Rust seules les valeurs des
    /// clés de [`RUST_FUNCTION_PATH_KEYS`] comptent (`#[cfg(feature = "x")]`
    /// et `#[serde(rename = "x")]` ne référencent rien) ; pour les
    /// décorateurs et annotations, les attributs de [`NON_SYMBOL_ATTRIBUTES`]
    /// sont ignorés.
    fn attribute_string_name<'s>(&self, source: &'s str, language: Language) -> Option<&'s str> {
        let k = self.k;
        if !k.has(self.node, flag::STRING_CONTENT) || !k.has_attribute {
            return None;
        }
        let mut found = false;
        for depth in 0..MAX_ATTRIBUTE_DEPTH {
            let Some(n) = self.up(depth) else {
                break;
            };
            if k.has(n, flag::ATTRIBUTE) {
                if language == Language::Rust {
                    if !self.is_rust_function_path_value(source) {
                        return None;
                    }
                } else if attribute_name(n, source)
                    .is_some_and(|a| NON_SYMBOL_ATTRIBUTES.contains(&a))
                {
                    return None;
                }
                found = true;
                break;
            }
        }
        if !found {
            return None;
        }
        let text = self.node.utf8_text(source.as_bytes()).ok()?;
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

    /// Classe une occurrence ; `None` = mot-clé à ne pas indexer (Elixir `def`).
    fn classify(&self, text: &str, source: &[u8], language: Language) -> Option<ReferenceKind> {
        if language == Language::Elixir
            && let Some(kind) = self.classify_elixir(text, source)
        {
            return kind;
        }
        // JSX : noms d'attributs, balises intrinsèques et fermantes ne sont
        // pas des symboles.
        if self.is_jsx_noise(text) {
            return None;
        }
        // Import d'abord : `import { Foo }` a un champ `name` dans un
        // `*_specifier`.
        Some(if self.in_import() {
            ReferenceKind::Import
        } else if self.is_jsx_tag_name() {
            ReferenceKind::Call
        } else if self.is_definition_name() {
            ReferenceKind::Definition
        } else if self.is_callee() {
            ReferenceKind::Call
        } else if self.is_type() {
            ReferenceKind::Type
        } else {
            ReferenceKind::Reference
        })
    }

    /// Elixir : les mots-clés `def*` / `alias` / `import`… sont des `call`
    /// dans l'AST. Retourne `Some(None)` pour le mot-clé lui-même (pas
    /// indexé), `Some(Some(Definition))` pour le nom qu'il déclare,
    /// `Some(Some(Import))` pour l'argument d'une directive, et `None` quand
    /// le classement générique s'applique.
    fn classify_elixir(&self, text: &str, source: &[u8]) -> Option<Option<ReferenceKind>> {
        let k = self.k;
        let node = self.node;
        let parent = self.up(0)?;
        // Le mot-clé : `def` dans `(call target: (identifier))`.
        if k.has(node, flag::IDENTIFIER)
            && k.has(parent, flag::CALL_NODE)
            && k.is_field(parent, node, k.target)
            && (ELIXIR_DEFINERS.contains(&text) || ELIXIR_DIRECTIVES.contains(&text))
        {
            return Some(None);
        }
        // Le nom déclaré : premier argument du mot-clé, directement (`def foo`,
        // `defmodule M`), comme cible d'un appel (`def foo(x)`) ou à gauche
        // d'un `when` (`def foo(x) when ...`). `head_depth` est la profondeur
        // de la tête au-dessus de la feuille.
        let head_depth = if k.has(parent, flag::ARGUMENTS) {
            0
        } else if k.has(parent, flag::CALL_NODE) && k.is_field(parent, node, k.target) {
            match self.up(1) {
                Some(op) if k.has(op, flag::BINARY_OPERATOR) && k.is_field(op, parent, k.left) => 2,
                _ => 1,
            }
        } else {
            return None;
        };
        let head = self.node_at(head_depth)?;
        let arguments = self.node_at(head_depth + 1)?;
        if !k.has(arguments, flag::ARGUMENTS) || arguments.named_child(0) != Some(head) {
            return None;
        }
        let keyword = self.node_at(head_depth + 2)?;
        if !k.has(keyword, flag::CALL_NODE) {
            return None;
        }
        let keyword_text = keyword
            .child_by_field_id(k.target)
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

    /// L'élément JSX (grammaire TSX) dont la feuille est la balise :
    /// `<Button>`, `<Ui.Card>` (dernier segment d'un `member_expression`).
    fn jsx_element_of(&self) -> Option<Node<'t>> {
        let k = self.k;
        let mut current = self.node;
        let mut depth = 0;
        while let Some(p) = self.up(depth)
            && k.has(p, flag::MEMBER_EXPRESSION)
        {
            if !k.is_field(p, current, k.property) {
                return None;
            }
            current = p;
            depth += 1;
        }
        self.up(depth)
            .filter(|p| k.has(*p, flag::JSX_ELEMENT) && k.is_field(*p, current, k.name))
    }

    /// JSX (grammaires TSX et JavaScript) : le nom d'un attribut (`onClick=`,
    /// `className=`), un nom à espace de noms (`<svg:rect>`, `xlink:href=`),
    /// une balise intrinsèque (`<div>`, `<motion.div>` : dernier segment en
    /// minuscules) et une balise fermante (`</Button>`, même usage que
    /// l'ouvrante) ne désignent aucun symbole du projet.
    fn is_jsx_noise(&self, text: &str) -> bool {
        let k = self.k;
        if self.up(0).is_some_and(|p| {
            k.has(p, flag::JSX_NAMESPACE_NAME)
                || (k.has(p, flag::JSX_ATTRIBUTE) && p.named_child(0) == Some(self.node))
        }) {
            return true;
        }
        // Balise fermante : tout son nom, `Ui` compris dans `</Ui.Card>`.
        let mut depth = 0;
        while let Some(a) = self.up(depth)
            && k.has(a, flag::MEMBER_EXPRESSION)
        {
            depth += 1;
        }
        if self
            .up(depth)
            .is_some_and(|a| k.has(a, flag::JSX_CLOSING_ELEMENT))
        {
            return true;
        }
        self.jsx_element_of().is_some() && text.starts_with(|c: char| c.is_lowercase())
    }

    /// Balise d'un composant JSX (`<Button />`, `<Ui.Card>`) : rendre le
    /// composant l'appelle (arête `App -> Button`).
    fn is_jsx_tag_name(&self) -> bool {
        self.jsx_element_of()
            .is_some_and(|element| !self.k.has(element, flag::JSX_CLOSING_ELEMENT))
    }

    /// Le nœud est-il le nom déclaré par une définition (`fn foo`, `class
    /// Foo`…) ?
    ///
    /// Le champ `name` existe aussi hors définitions (`Foo::new`, `obj.meth()`
    /// en Java), d'où le filtre sur le type du parent. Un `export_specifier`
    /// (`export { foo }`) réexporte un nom sans le définir.
    fn is_definition_name(&self) -> bool {
        let k = self.k;
        let node = self.node;
        let Some(parent) = self.up(0) else {
            return false;
        };
        if k.has(parent, flag::EXPORT_SPECIFIER) {
            return false;
        }

        // C/C++ : `int foo(...)` → function_declarator(declarator: identifier)
        if k.has(parent, flag::FUNCTION_DECLARATOR) {
            return k.is_field(parent, node, k.declarator);
        }
        // Bash : `foo() { ... }`
        if k.has(parent, flag::FUNCTION_DEFINITION) && k.has(node, flag::WORD) {
            return true;
        }

        k.has(parent, flag::DECLARES) && k.is_field(parent, node, k.name)
    }

    fn in_import(&self) -> bool {
        let k = self.k;
        for depth in 0..MAX_IMPORT_DEPTH {
            let Some(n) = self.up(depth) else {
                return false;
            };
            if k.has(n, flag::IMPORT_CONTEXT)
                // `export { a } from './a'` : réexport d'un nom importé.
                || (k.has(n, flag::EXPORT_STATEMENT) && n.child_by_field_id(k.source).is_some())
            {
                return true;
            }
        }
        false
    }

    /// Le nœud est-il ce qui est appelé (et non un argument ou le receveur) ?
    fn is_callee(&self) -> bool {
        let k = self.k;
        // Remonte la partie droite des accès membres : dans `obj.meth()`,
        // c'est `meth` qui est appelé, pas `obj`.
        let mut current = self.node;
        let mut depth = 0;
        while let Some(p) = self.up(depth) {
            if !k.has(p, flag::MEMBER_ACCESS) {
                break;
            }
            let is_last = p.named_child(p.named_child_count().saturating_sub(1)) == Some(current);
            if !is_last {
                return false;
            }
            current = p;
            depth += 1;
        }

        let Some(call) = self.up(depth) else {
            return false;
        };
        // Rust : les arguments d'une macro (`assert_eq!(f(x), 1)`, `vec![g()]`,
        // `tracing::info!(n = h())`) ne sont pas parsés en expressions.
        if k.has(call, flag::TOKEN_TREE) {
            return self.is_macro_argument_call(current, depth);
        }
        if !k.has(call, flag::CALL) {
            return false;
        }

        if k.callee.iter().any(|&f| k.is_field(call, current, f)) {
            return true;
        }
        // Grammaires sans champ nommé (Kotlin, Scala, PHP `new`, Bash) :
        // l'appelé est le premier enfant, à condition que le receveur ne soit
        // pas un champ.
        !k.receiver.iter().any(|&f| k.is_field(call, current, f))
            && call.named_child(0) == Some(current)
    }

    /// Rust : un identifiant dans le `token_tree` d'une `macro_invocation`
    /// est un appel quand il est suivi de `(` (`f(x)`) ou de `!(` (macro
    /// imbriquée). Volontairement conservateur : `x = f()` donne `f` seul,
    /// `Foo::bar()` donne `bar` seul. Les `token_tree` d'attributs
    /// (`#[cfg(all(...))]`) et de `macro_rules!` ne sont pas concernés.
    /// `node` est le nœud dont le parent, `up(depth)`, est le `token_tree`.
    fn is_macro_argument_call(&self, node: Node<'t>, depth: usize) -> bool {
        let k = self.k;
        let mut outer_depth = depth;
        while let Some(tree) = self.up(outer_depth) {
            if !k.has(tree, flag::TOKEN_TREE) {
                break;
            }
            outer_depth += 1;
        }
        if !self
            .up(outer_depth)
            .is_some_and(|n| k.has(n, flag::MACRO_INVOCATION))
        {
            return false;
        }
        let Some(token_tree) = self.up(depth) else {
            return false;
        };
        let (_, mut next) = siblings(token_tree, node);
        if next.is_some_and(|n| k.has(n, flag::BANG)) {
            next = next.and_then(|n| siblings(token_tree, n).1);
        }
        next.is_some_and(|n| {
            k.has(n, flag::TOKEN_TREE) && n.child(0).is_some_and(|c| k.has(c, flag::LPAREN))
        })
    }

    fn is_type(&self) -> bool {
        let k = self.k;
        if k.has(self.node, flag::TYPE_IDENTIFIER) {
            return true;
        }
        self.up(0).is_some_and(|p| k.has(p, flag::TYPE_CONTEXT))
    }
}

/// Mots-clés Elixir qui déclarent un symbole : `def foo(x)` est un `call` dont
/// le `target` est `def` et dont le premier argument porte le nom déclaré.
/// Même liste que `queries/elixir/tags.scm`, plus `defimpl` et les mots-clés
/// sans nom déclaré (`defstruct`, `defexception`, `defoverridable`).
const ELIXIR_DEFINERS: [&str; 15] = [
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
    "defexception",
    "defoverridable",
];

/// Directives Elixir : leurs arguments sont des imports, pas des appels.
const ELIXIR_DIRECTIVES: [&str; 4] = ["alias", "import", "use", "require"];

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
            "defmodule Mod do\n  def foo(x) do\n    bar(x)\n  end\n  defp bar(x), do: x\n  def baz(x) when is_integer(x), do: x\n  def noparen, do: :ok\n  alias A.B\n  import Enum\n  defstruct [:name]\n  defexception [:message]\n  defoverridable foo: 1\nend\n",
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
        for keyword in [
            "defmodule",
            "def",
            "defp",
            "alias",
            "import",
            "defstruct",
            "defexception",
            "defoverridable",
        ] {
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
            "fn default_true() -> bool { true }\n#[derive(serde::Deserialize)]\n#[serde(rename_all = \"camelCase\", tag = \"kind\")]\npub struct Cfg {\n    #[serde(default = \"default_true\", skip_serializing_if = \"is_false\")]\n    pub on: bool,\n    #[serde(with = \"my_mod::as_str\", rename = \"wire_name\")]\n    pub w: u8,\n}\n#[doc = \"hidden\"]\n#[cfg(feature = \"onnx\")]\n#[validate(custom(function = \"check\"))]\nfn g() { let s = \"default_true\"; }\n",
        );
        assert_eq!(
            kinds_of(&r, "default_true"),
            vec![
                (1, ReferenceKind::Definition),
                (5, ReferenceKind::Reference)
            ]
        );
        assert_eq!(
            kinds_of(&r, "is_false"),
            vec![(5, ReferenceKind::Reference)]
        );
        assert_eq!(kinds_of(&r, "as_str"), vec![(7, ReferenceKind::Reference)]);
        assert_eq!(kinds_of(&r, "check"), vec![(12, ReferenceKind::Reference)]);
        assert!(kinds_of(&r, "my_mod::as_str").is_empty());
        // Keys that name strings, not symbols.
        for name in ["camelCase", "kind", "wire_name", "hidden", "onnx"] {
            assert!(kinds_of(&r, name).is_empty(), "{name} referenced");
        }
    }

    #[test]
    fn test_rust_attribute_scope_pins() {
        let r = refs(
            Language::Rust,
            "#[cfg(feature = \"xx\")]\nfn a() {}\n#[serde(rename = \"xx\")]\nfn b() {}\n#[serde(default = \"ff\")]\nfn c() {}\n",
        );
        assert!(kinds_of(&r, "xx").is_empty());
        assert_eq!(kinds_of(&r, "ff"), vec![(5, ReferenceKind::Reference)]);
    }

    #[test]
    fn test_decorator_and_annotation_strings_reference_names() {
        let py = refs(
            Language::Python,
            "@pytest.fixture(name=\"session_db\")\ndef db(): pass\n@app.route(\"/users\")\ndef users(): return \"session_db\"\n@pytest.mark.parametrize(\"value\", [1])\ndef t(value): pass\n",
        );
        assert_eq!(
            kinds_of(&py, "session_db"),
            vec![(1, ReferenceKind::Reference)]
        );
        assert!(kinds_of(&py, "/users").is_empty());
        // `parametrize` names a parameter: only the parameter itself counts.
        assert_eq!(kinds_of(&py, "value"), vec![(6, ReferenceKind::Reference)]);

        let java = refs(
            Language::Java,
            "class C {\n  @DependsOn(\"initCache\")\n  @JsonProperty(\"user_id\")\n  void run() { String s = \"initCache\"; }\n}",
        );
        assert_eq!(
            kinds_of(&java, "initCache"),
            vec![(2, ReferenceKind::Reference)]
        );
        assert!(kinds_of(&java, "user_id").is_empty());

        let csharp = refs(
            Language::CSharp,
            "class C {\n  [DependsOn(\"initCache\")]\n  [JsonPropertyName(\"user_id\")]\n  void Run() { var s = \"initCache\"; }\n}",
        );
        assert_eq!(
            kinds_of(&csharp, "initCache"),
            vec![(2, ReferenceKind::Reference)]
        );
        assert!(kinds_of(&csharp, "user_id").is_empty());

        let kotlin = refs(
            Language::Kotlin,
            "class C {\n  @Named(\"initCache\")\n  @Suppress(\"UNCHECKED_CAST\")\n  fun run() { val s = \"initCache\" }\n}",
        );
        assert_eq!(
            kinds_of(&kotlin, "initCache"),
            vec![(2, ReferenceKind::Reference)]
        );
        assert!(kinds_of(&kotlin, "UNCHECKED_CAST").is_empty());
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
