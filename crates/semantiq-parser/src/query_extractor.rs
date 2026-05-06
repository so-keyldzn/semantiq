use crate::language::Language;
use crate::symbols::{Symbol, SymbolKind};
use anyhow::{Context, Result};
use std::collections::HashMap;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, Tree};

/// Extracteur de symboles basé sur des tree-sitter queries.
///
/// Cette approche remplace la traversée AST récursive par des pattern matching
/// déclaratifs, ce qui est plus maintenable et extensible.
pub struct QuerySymbolExtractor {
    /// Queries compilées par langage. Une query est immutable et thread-safe.
    queries: HashMap<Language, Query>,
}

impl QuerySymbolExtractor {
    /// Crée un nouvel extracteur en compilant les queries pour chaque langage supporté.
    ///
    /// La compilation d'une query est coûteuse (~ms), elle n'est donc faite qu'une seule fois.
    pub fn new() -> Result<Self> {
        let mut queries = HashMap::new();

        // Query Rust — compilée depuis le fichier .scm embarqué
        let rust_query_source = include_str!("../queries/rust/tags.scm");
        let rust_lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let query = Query::new(&rust_lang, rust_query_source)
            .map_err(|e| anyhow::anyhow!("Failed to compile Rust query: {:?}", e))?;
        queries.insert(Language::Rust, query);

        Ok(Self { queries })
    }

    /// Extrait les symboles d'un arbre syntaxique en utilisant la query du langage.
    ///
    /// Processus :
    /// 1. Exécute la query sur l'arbre
    /// 2. Pour chaque match, extrait le nom et le kind depuis les captures
    /// 3. Post-traitement : résolution des parents, signatures, doc comments
    pub fn extract(&self, tree: &Tree, source: &str, language: Language) -> Result<Vec<Symbol>> {
        let query = self
            .queries
            .get(&language)
            .context("No query available for language")?;

        let mut cursor = QueryCursor::new();
        let source_bytes = source.as_bytes();

        // Collecte toutes les captures pertinentes
        // Clé: (start_byte, end_byte) pour détecter les doublons
        let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
        let mut raw_symbols: Vec<RawSymbol> = Vec::new();

        let mut matches = cursor.matches(query, tree.root_node(), source_bytes);
        while let Some(m) = matches.next() {
            let mut name: Option<String> = None;
            let mut kind: Option<SymbolKind> = None;
            let mut definition_node: Option<Node> = None;

            for capture in m.captures {
                let capture_name = query.capture_names()[capture.index as usize];

                match capture_name {
                    "name" => {
                        name = Some(capture.node.utf8_text(source_bytes)?.to_string());
                    }
                    name if name.starts_with("definition.") => {
                        kind = Self::capture_to_kind(name, capture.node.kind());
                        definition_node = Some(capture.node);
                    }
                    _ => {} // Ignore les autres captures
                }
            }

            // Pour use_declaration, on n'a pas de @name capture
            // On extrait le nom depuis le texte du nœud
            if name.is_none()
                && let Some(node) = definition_node
                    && let Some(SymbolKind::Import) = kind {
                        name = Some(node.utf8_text(source_bytes)?.to_string());
                    }

            if let (Some(name), Some(kind), Some(node)) = (name, kind, definition_node) {
                // Déduplication: un même nœud peut matcher plusieurs patterns
                // (ex: function_item dans source_file ET dans declaration_list)
                // On garde le premier match (déterministe car trié par pattern index)
                // La clé est le start_byte + end_byte du nœud de définition
                let key = (node.start_byte(), node.end_byte());
                if seen.insert(key) {
                    raw_symbols.push(RawSymbol { name, kind, node });
                }
            }
        }

        // Tri par position pour un ordre déterministe
        raw_symbols.sort_by_key(|s| s.node.start_byte());

        // Construction des Symbol avec post-traitement
        let mut symbols = Vec::new();
        for raw in raw_symbols {
            let parent = Self::resolve_parent(&raw.node, source, language);
            let signature = Self::extract_signature(&raw.node, source);
            let doc_comment = Self::extract_doc_comment(&raw.node, source);

            symbols.push(Symbol {
                name: raw.name,
                kind: raw.kind,
                start_line: raw.node.start_position().row + 1,
                end_line: raw.node.end_position().row + 1,
                start_byte: raw.node.start_byte(),
                end_byte: raw.node.end_byte(),
                signature,
                doc_comment,
                parent,
            });
        }

        Ok(symbols)
    }

    /// Mapping capture name → SymbolKind.
    ///
    /// Pour Rust, on utilise les captures spécifiques (@definition.struct, etc.).
    fn capture_to_kind(capture_name: &str, _node_kind: &str) -> Option<SymbolKind> {
        match capture_name {
            "definition.function" => Some(SymbolKind::Function),
            "definition.method" => Some(SymbolKind::Method),
            "definition.class" => Some(SymbolKind::Class),
            "definition.struct" => Some(SymbolKind::Struct),
            "definition.enum" => Some(SymbolKind::Enum),
            "definition.interface" => Some(SymbolKind::Interface),
            "definition.trait" => Some(SymbolKind::Trait),
            "definition.module" => Some(SymbolKind::Module),
            "definition.variable" => Some(SymbolKind::Variable),
            "definition.constant" => Some(SymbolKind::Constant),
            "definition.type" => Some(SymbolKind::Type),
            "definition.import" => Some(SymbolKind::Import),
            "definition.macro" => Some(SymbolKind::Function), // Macro mapped to Function for now
            _ => None,
        }
    }

    /// Résout le chemin hiérarchique parent d'un nœud.
    ///
    /// Remonte l'arbre depuis le nœud et collecte les noms des conteneurs.
    fn resolve_parent(node: &Node, source: &str, language: Language) -> Option<String> {
        let source_bytes = source.as_bytes();
        let mut current = node.parent();
        let mut path_segments = Vec::new();

        while let Some(parent) = current {
            if let Some(parent_name) = Self::container_name(&parent, source_bytes, language) {
                path_segments.push(parent_name);
            }
            current = parent.parent();
        }

        if path_segments.is_empty() {
            None
        } else {
            path_segments.reverse();
            Some(path_segments.join("::"))
        }
    }

    /// Extrait le nom d'un nœud conteneur (struct, enum, trait, impl, mod).
    fn container_name(node: &Node, source_bytes: &[u8], language: Language) -> Option<String> {
        match language {
            Language::Rust => match node.kind() {
                "impl_item" => {
                    // impl Foo or impl Trait for Foo
                    node.child_by_field_name("type")
                        .and_then(|n| n.utf8_text(source_bytes).ok())
                        .map(|s| s.to_string())
                }
                "struct_item" | "enum_item" | "trait_item" | "mod_item" => node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source_bytes).ok())
                    .map(|s| s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Extrait la signature (première ligne du nœud).
    fn extract_signature(node: &Node, source: &str) -> Option<String> {
        let source_bytes = source.as_bytes();
        let text = node.utf8_text(source_bytes).ok()?;
        let first_line = text.lines().next()?;
        let sig = if first_line.chars().count() > 200 {
            let truncated: String = first_line.chars().take(200).collect();
            format!("{}...", truncated)
        } else {
            first_line.to_string()
        };
        Some(sig.trim().to_string())
    }

    /// Extrait les doc comments précédant le nœud.
    fn extract_doc_comment(node: &Node, source: &str) -> Option<String> {
        let source_bytes = source.as_bytes();
        let mut prev = node.prev_sibling();
        let mut comments = Vec::new();

        while let Some(sibling) = prev {
            if sibling.kind().contains("comment") {
                if let Ok(comment) = sibling.utf8_text(source_bytes) {
                    comments.push(comment.to_string());
                }
                prev = sibling.prev_sibling();
            } else {
                break;
            }
        }

        if comments.is_empty() {
            None
        } else {
            comments.reverse();
            Some(comments.join("\n"))
        }
    }
}

/// Représentation intermédiaire d'un symbole extrait par query.
struct RawSymbol<'a> {
    name: String,
    kind: SymbolKind,
    node: Node<'a>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::{Language, LanguageSupport};
    use crate::symbols::SymbolExtractor;

    #[test]
    fn test_query_rust_basic() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
/// A greeting function
fn hello(name: &str) -> String {
    format!("Hello, {}!", name)
}

struct User {
    name: String,
    age: u32,
}

impl User {
    fn new(name: String) -> Self {
        Self { name, age: 0 }
    }
}

enum Status {
    Active,
    Inactive,
}

trait Drawable {
    fn draw(&self);
}

mod utils {
    pub fn helper() {}
}

const MAX_SIZE: usize = 100;
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let extractor = QuerySymbolExtractor::new().unwrap();
        let symbols = extractor.extract(&tree, source, Language::Rust).unwrap();

        // Vérifier les symboles extraits
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "hello" && s.kind == SymbolKind::Function),
            "hello should be extracted as Function"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "User" && s.kind == SymbolKind::Struct),
            "User should be extracted as Struct"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "new" && s.kind == SymbolKind::Method),
            "new should be extracted as Method"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Status" && s.kind == SymbolKind::Enum),
            "Status should be extracted as Enum"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Drawable" && s.kind == SymbolKind::Trait),
            "Drawable should be extracted as Trait"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "utils" && s.kind == SymbolKind::Module),
            "utils should be extracted as Module"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "MAX_SIZE" && s.kind == SymbolKind::Constant),
            "MAX_SIZE should be extracted as Constant"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "helper" && s.kind == SymbolKind::Function),
            "helper in mod should be extracted as Function, not Method"
        );
        assert!(
            !symbols.iter().any(|s| s.kind == SymbolKind::Class),
            "No impl_item should be captured as Class"
        );
    }

    #[test]
    fn test_query_vs_legacy_rust() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
/// A greeting function
fn hello(name: &str) -> String {
    format!("Hello, {}!", name)
}

struct User {
    name: String,
    age: u32,
}

impl User {
    fn new(name: String) -> Self {
        Self { name, age: 0 }
    }
}

enum Status {
    Active,
    Inactive,
}

trait Drawable {
    fn draw(&self);
}

mod utils {
    pub fn helper() {}
}

const MAX_SIZE: usize = 100;
"#;
        let tree = support.parse(Language::Rust, source).unwrap();

        // Legacy extraction — filter out impl_item (captured as Class in legacy, excluded in query)
        let legacy_symbols: Vec<_> = SymbolExtractor::extract_legacy(&tree, source, Language::Rust)
            .unwrap()
            .into_iter()
            .filter(|s| !(s.kind == SymbolKind::Class && s.parent.is_none()))
            .collect();

        // Query extraction
        let query_extractor = QuerySymbolExtractor::new().unwrap();
        let query_symbols = query_extractor
            .extract(&tree, source, Language::Rust)
            .unwrap();

        // Compare: même nombre de symboles
        assert_eq!(
            query_symbols.len(),
            legacy_symbols.len(),
            "Query and legacy should extract same number of symbols.\nQuery: {:?}\nLegacy: {:?}",
            query_symbols
                .iter()
                .map(|s| (&s.name, s.kind))
                .collect::<Vec<_>>(),
            legacy_symbols
                .iter()
                .map(|s| (&s.name, s.kind))
                .collect::<Vec<_>>(),
        );

        // Vérifier que chaque symbole legacy a un équivalent query
        for legacy in &legacy_symbols {
            // Legacy: fonctions dans impl/mod sont Function (traversée récursive)
            // Query: fonctions dans impl sont Method, fonctions dans mod sont Function
            let found = if legacy.kind == SymbolKind::Function && legacy.parent.is_some() {
                query_symbols.iter().any(|q| {
                    q.name == legacy.name
                        && (q.kind == SymbolKind::Function || q.kind == SymbolKind::Method)
                        && q.parent == legacy.parent
                })
            } else {
                query_symbols.iter().any(|q| {
                    q.name == legacy.name && q.kind == legacy.kind && q.parent == legacy.parent
                })
            };
            assert!(
                found,
                "Legacy symbol {:?} {} not found in query extraction",
                legacy.kind, legacy.name
            );
        }
    }

    #[test]
    fn test_query_rust_parent_resolution() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
impl Calculator {
    fn add(&self, n: i32) -> i32 {
        self.value + n
    }
}
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let extractor = QuerySymbolExtractor::new().unwrap();
        let symbols = extractor.extract(&tree, source, Language::Rust).unwrap();

        let add_func = symbols.iter().find(|s| s.name == "add").unwrap();
        assert_eq!(add_func.parent.as_deref(), Some("Calculator"));
        assert_eq!(add_func.kind, SymbolKind::Method);
    }

    #[test]
    fn test_query_rust_doc_comments() {
        let mut support = LanguageSupport::new().unwrap();
        let source = r#"
/// This is a documented function
/// It does something important
fn documented_function() {
    println!("Hello");
}
"#;
        let tree = support.parse(Language::Rust, source).unwrap();
        let extractor = QuerySymbolExtractor::new().unwrap();
        let symbols = extractor.extract(&tree, source, Language::Rust).unwrap();

        let func = symbols
            .iter()
            .find(|s| s.name == "documented_function")
            .unwrap();
        assert!(func.doc_comment.is_some());
        assert!(
            func.doc_comment
                .as_ref()
                .unwrap()
                .contains("documented function")
        );
    }
}
