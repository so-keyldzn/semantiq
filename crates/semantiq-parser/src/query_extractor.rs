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
    /// **Comportement strict** : toute query qui échoue à compiler fait échouer l'initialisation
    /// avec la liste exhaustive des erreurs. Une .scm cassée est une erreur de programmation,
    /// pas un état runtime à dégrader silencieusement.
    pub fn new() -> Result<Self> {
        let mut queries = HashMap::new();
        let mut errors: Vec<String> = Vec::new();

        // Macro locale pour compiler une query et accumuler les erreurs.
        macro_rules! load {
            ($lang:expr, $path:literal, $grammar:expr) => {{
                let src = include_str!($path);
                let ts_lang: tree_sitter::Language = $grammar.into();
                match Query::new(&ts_lang, src) {
                    Ok(q) => {
                        queries.insert($lang, q);
                    }
                    Err(e) => {
                        errors.push(format!(
                            "  - {} ({}): {:?}",
                            $lang.name(),
                            $path,
                            e
                        ));
                    }
                }
            }};
        }

        load!(Language::Rust, "../queries/rust/tags.scm", tree_sitter_rust::LANGUAGE);
        load!(
            Language::TypeScript,
            "../queries/typescript/tags.scm",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT
        );
        load!(
            Language::JavaScript,
            "../queries/javascript/tags.scm",
            tree_sitter_javascript::LANGUAGE
        );
        load!(
            Language::Python,
            "../queries/python/tags.scm",
            tree_sitter_python::LANGUAGE
        );
        load!(Language::Go, "../queries/go/tags.scm", tree_sitter_go::LANGUAGE);
        load!(
            Language::Java,
            "../queries/java/tags.scm",
            tree_sitter_java::LANGUAGE
        );
        load!(Language::C, "../queries/c/tags.scm", tree_sitter_c::LANGUAGE);
        load!(Language::Cpp, "../queries/cpp/tags.scm", tree_sitter_cpp::LANGUAGE);
        load!(
            Language::Php,
            "../queries/php/tags.scm",
            tree_sitter_php::LANGUAGE_PHP
        );
        load!(
            Language::Ruby,
            "../queries/ruby/tags.scm",
            tree_sitter_ruby::LANGUAGE
        );
        load!(
            Language::CSharp,
            "../queries/csharp/tags.scm",
            tree_sitter_c_sharp::LANGUAGE
        );
        load!(
            Language::Kotlin,
            "../queries/kotlin/tags.scm",
            tree_sitter_kotlin_ng::LANGUAGE
        );
        load!(
            Language::Scala,
            "../queries/scala/tags.scm",
            tree_sitter_scala::LANGUAGE
        );
        load!(
            Language::Html,
            "../queries/html/tags.scm",
            tree_sitter_html::LANGUAGE
        );
        load!(
            Language::Json,
            "../queries/json/tags.scm",
            tree_sitter_json::LANGUAGE
        );
        load!(
            Language::Yaml,
            "../queries/yaml/tags.scm",
            tree_sitter_yaml::LANGUAGE
        );
        load!(
            Language::Toml,
            "../queries/toml/tags.scm",
            tree_sitter_toml_ng::LANGUAGE
        );
        load!(
            Language::Bash,
            "../queries/bash/tags.scm",
            tree_sitter_bash::LANGUAGE
        );
        load!(
            Language::Elixir,
            "../queries/elixir/tags.scm",
            tree_sitter_elixir::LANGUAGE
        );

        if !errors.is_empty() {
            return Err(anyhow::anyhow!(
                "Tree-sitter query compilation failed for {} language(s):\n{}",
                errors.len(),
                errors.join("\n")
            ));
        }

        Ok(Self { queries })
    }

    /// Retourne true si une query est disponible pour ce langage.
    pub fn has_query(&self, language: Language) -> bool {
        self.queries.contains_key(&language)
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

        // Déduplication par (start_byte, end_byte) avec priorité au plus petit
        // pattern_index. Tree-sitter émet les matches dans un ordre dicté par la
        // complétion du pattern dans l'arbre, qui n'est PAS l'ordre déclaré dans
        // le .scm. Pour qu'un pattern plus spécifique (déclaré en premier) gagne
        // sur un pattern générique (déclaré plus bas) sur le même nœud, on garde
        // celui avec le plus petit pattern_index.
        let mut by_range: std::collections::HashMap<(usize, usize), (u32, RawSymbol)> =
            std::collections::HashMap::new();

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

            // Pour les imports capturés sans @name explicite (ex: Rust use_declaration,
            // Java import_declaration, PHP namespace_use_declaration), on extrait le
            // nom court (dernier segment du path) plutôt que le texte entier — ça
            // évite de polluer FTS5 avec des chaînes type "use std::collections::HashMap;".
            if name.is_none()
                && let Some(node) = definition_node
                && let Some(SymbolKind::Import) = kind
            {
                name = Self::extract_import_short_name(&node, source_bytes);
            }

            if let (Some(name), Some(kind), Some(node)) = (name, kind, definition_node) {
                let key = (node.start_byte(), node.end_byte());
                let pattern_idx = m.pattern_index as u32;
                match by_range.get(&key) {
                    Some((existing_idx, _)) if *existing_idx <= pattern_idx => {
                        // Match existant prioritaire (pattern_index plus petit ou égal) — on l'ignore.
                    }
                    _ => {
                        by_range.insert(key, (pattern_idx, RawSymbol { name, kind, node }));
                    }
                }
            }
        }

        let mut raw_symbols: Vec<RawSymbol> =
            by_range.into_values().map(|(_, sym)| sym).collect();
        raw_symbols.sort_by_key(|s| s.node.start_byte());

        // Construction des Symbol avec post-traitement
        let mut symbols = Vec::new();
        for raw in raw_symbols {
            let parent = Self::resolve_parent(&raw.node, source, language);
            let signature = Self::extract_signature(&raw.node, source);
            let doc_comment = Self::extract_doc_comment(&raw.node, source);
            let kind = Self::post_process_kind(raw.kind, &raw.node, language);

            symbols.push(Symbol {
                name: raw.name,
                kind,
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

    /// Post-traitement spécifique au langage pour ajuster le SymbolKind.
    ///
    /// Cas géré : TypeScript / JavaScript — un `lexical_declaration` ou
    /// `variable_declaration` capturé comme Variable est promu Function si sa
    /// valeur est `arrow_function` ou `function_expression`. Mirroring de
    /// `SymbolExtractor::is_function_variable` du chemin legacy.
    fn post_process_kind(kind: SymbolKind, node: &Node, language: Language) -> SymbolKind {
        if matches!(kind, SymbolKind::Variable)
            && matches!(language, Language::TypeScript | Language::JavaScript)
            && Self::is_function_variable(node)
        {
            return SymbolKind::Function;
        }
        kind
    }

    /// Vérifie si un lexical_declaration / variable_declaration contient une
    /// arrow_function ou function_expression comme valeur.
    fn is_function_variable(node: &Node) -> bool {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "variable_declarator"
                && let Some(value) = child.child_by_field_name("value")
                && matches!(value.kind(), "arrow_function" | "function_expression")
            {
                return true;
            }
        }
        false
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
    /// Le séparateur dépend de la convention idiomatique du langage :
    /// `.` pour Elixir (`MyApp.User`), `::` pour la plupart des autres.
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
            Some(path_segments.join(Self::parent_separator(language)))
        }
    }

    fn parent_separator(language: Language) -> &'static str {
        match language {
            // Conventions idiomatiques :
            // - Elixir utilise `.` (ex: `MyApp.User`)
            // - JSON/YAML/TOML utilisent `.` pour les paths de configuration
            Language::Elixir | Language::Json | Language::Yaml | Language::Toml => ".",
            _ => "::",
        }
    }

    /// Extrait le nom d'un nœud conteneur pour la résolution de parent hiérarchique.
    /// Retourne `Some(name)` si le nœud est un conteneur de symboles (class, module, etc.).
    fn container_name(node: &Node, source_bytes: &[u8], language: Language) -> Option<String> {
        let read_field = |field: &str| -> Option<String> {
            node.child_by_field_name(field)
                .and_then(|n| n.utf8_text(source_bytes).ok())
                .map(|s| s.to_string())
        };

        match language {
            Language::Rust => match node.kind() {
                "impl_item" => read_field("type"),
                "struct_item" | "enum_item" | "trait_item" | "mod_item" => read_field("name"),
                _ => None,
            },
            Language::TypeScript | Language::JavaScript => match node.kind() {
                "class_declaration"
                | "abstract_class_declaration"
                | "interface_declaration"
                | "enum_declaration" => read_field("name"),
                _ => None,
            },
            Language::Python => match node.kind() {
                "class_definition" | "function_definition" => read_field("name"),
                _ => None,
            },
            Language::Go => match node.kind() {
                "type_spec" => read_field("name"),
                _ => None,
            },
            Language::Java => match node.kind() {
                "class_declaration" | "interface_declaration" | "enum_declaration" => {
                    read_field("name")
                }
                _ => None,
            },
            Language::C | Language::Cpp => match node.kind() {
                "class_specifier" | "struct_specifier" | "union_specifier"
                | "namespace_definition" => read_field("name"),
                _ => None,
            },
            Language::Php => match node.kind() {
                "class_declaration"
                | "interface_declaration"
                | "trait_declaration"
                | "enum_declaration"
                | "namespace_definition" => read_field("name"),
                _ => None,
            },
            Language::Ruby => match node.kind() {
                "class" | "module" => {
                    // name field can be either a constant or a scope_resolution
                    node.child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source_bytes).ok())
                        .map(|s| s.to_string())
                }
                _ => None,
            },
            Language::CSharp => match node.kind() {
                "class_declaration"
                | "struct_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "namespace_declaration"
                | "file_scoped_namespace_declaration" => read_field("name"),
                _ => None,
            },
            Language::Kotlin => match node.kind() {
                "class_declaration" | "object_declaration" => {
                    // tree-sitter-kotlin-ng expose le nom via le premier `identifier`
                    // enfant direct (pas un type_identifier ni un field nommé).
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        if child.kind() == "identifier"
                            && let Ok(text) = child.utf8_text(source_bytes)
                        {
                            return Some(text.to_string());
                        }
                    }
                    None
                }
                _ => None,
            },
            Language::Scala => match node.kind() {
                "class_definition" | "object_definition" | "trait_definition"
                | "enum_definition" => read_field("name"),
                _ => None,
            },
            Language::Json => {
                // JSON pair → key string. Strip surrounding quotes for clean parent path.
                if node.kind() == "pair"
                    && let Some(key) = node.child_by_field_name("key")
                    && let Ok(text) = key.utf8_text(source_bytes)
                {
                    return Some(text.trim_matches('"').to_string());
                }
                None
            }
            Language::Yaml => {
                // YAML block_mapping_pair → key flow_node text.
                if node.kind() == "block_mapping_pair"
                    && let Some(key) = node.child_by_field_name("key")
                    && let Ok(text) = key.utf8_text(source_bytes)
                {
                    return Some(text.to_string());
                }
                None
            }
            Language::Toml => {
                // TOML : seules les `table` ([header]) et `table_array_element`
                // ([[header]]) servent de conteneur pour les pair imbriqués. La
                // dotted_key (`a.b.c`) est déjà au format dot-separated.
                if matches!(node.kind(), "table" | "table_array_element") {
                    let mut walker = node.walk();
                    for child in node.children(&mut walker) {
                        if matches!(child.kind(), "bare_key" | "dotted_key")
                            && let Ok(text) = child.utf8_text(source_bytes)
                        {
                            return Some(text.to_string());
                        }
                    }
                }
                None
            }
            Language::Elixir => {
                // Reconnaître `defmodule Foo do … end` comme conteneur. Structure AST :
                //   call
                //     identifier "defmodule"   ← premier child positionnel
                //     arguments
                //       alias "Foo"
                //     do_block …
                //
                // Le grammar tree-sitter-elixir n'expose pas toujours de field `target`,
                // on utilise donc une traversée positionnelle des named children.
                if node.kind() == "call" {
                    let mut walker = node.walk();
                    let mut named = node.children(&mut walker).filter(|c| c.is_named());
                    let target = named.next();
                    let arguments = named.next();
                    if let (Some(target), Some(arguments)) = (target, arguments)
                        && target.kind() == "identifier"
                        && let Ok(target_text) = target.utf8_text(source_bytes)
                        && target_text == "defmodule"
                    {
                        let mut arg_walker = arguments.walk();
                        for child in arguments.children(&mut arg_walker) {
                            if child.kind() == "alias"
                                && let Ok(text) = child.utf8_text(source_bytes)
                            {
                                return Some(text.to_string());
                            }
                        }
                    }
                }
                None
            }
            // Languages without meaningful nesting (HTML, JSON, YAML, TOML, Bash) — no parent resolution
            _ => None,
        }
    }

    /// Extrait un nom court pour un nœud d'import dont la query n'a pas de @name.
    ///
    /// Stratégie : DFS pour trouver le DERNIER nœud terminal qui ressemble à un
    /// identifier (kind `identifier`, `name`, `type_identifier`, etc.). Cela donne
    /// `HashMap` pour `use std::collections::HashMap;`, `Bar` pour `use Foo\Bar;`,
    /// etc.
    fn extract_import_short_name(node: &Node, source_bytes: &[u8]) -> Option<String> {
        fn is_name_kind(kind: &str) -> bool {
            matches!(
                kind,
                "identifier" | "name" | "type_identifier" | "field_identifier"
            )
        }

        fn find_last(node: Node, source_bytes: &[u8], best: &mut Option<String>) {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if is_name_kind(child.kind())
                    && let Ok(text) = child.utf8_text(source_bytes)
                {
                    *best = Some(text.to_string());
                }
                find_last(child, source_bytes, best);
            }
        }

        let mut best: Option<String> = None;
        find_last(*node, source_bytes, &mut best);
        // Fallback : si rien trouvé, retomber sur le texte entier (nettoyé).
        best.or_else(|| {
            node.utf8_text(source_bytes)
                .ok()
                .map(|s| s.trim_end_matches(';').trim().to_string())
        })
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

    // -------------------------------------------------------------------------
    // Tests de couverture pour les 18 langages migrés vers les queries.
    // Pour chaque langage : on vérifie qu'une query est enregistrée puis on
    // extrait des symboles d'un snippet représentatif et on vérifie les kinds.
    // -------------------------------------------------------------------------

    #[test]
    fn test_all_supported_languages_have_query() {
        let extractor = QuerySymbolExtractor::new().unwrap();
        for lang in LanguageSupport::supported_languages() {
            assert!(
                extractor.has_query(*lang),
                "Query should be registered for {:?}",
                lang
            );
        }
    }

    fn extract_via_query(lang: Language, source: &str) -> Vec<crate::symbols::Symbol> {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(lang, source).unwrap();
        let extractor = QuerySymbolExtractor::new().unwrap();
        extractor.extract(&tree, source, lang).unwrap()
    }

    #[test]
    fn test_query_typescript() {
        let source = r#"
interface User { name: string; }
type Id = string;
enum Color { Red, Blue }
class Calculator {
    add(n: number): number { return n + 1; }
}
function greet(name: string): string { return name; }
const fadeIn = () => 1;
const cfg = { debug: true };
import { foo } from "./bar";
"#;
        let symbols = extract_via_query(Language::TypeScript, source);

        assert!(symbols.iter().any(|s| s.name == "User" && s.kind == SymbolKind::Interface));
        assert!(symbols.iter().any(|s| s.name == "Id" && s.kind == SymbolKind::Type));
        assert!(symbols.iter().any(|s| s.name == "Color" && s.kind == SymbolKind::Enum));
        assert!(symbols.iter().any(|s| s.name == "Calculator" && s.kind == SymbolKind::Class));
        assert!(symbols.iter().any(|s| s.name == "add" && s.kind == SymbolKind::Method));
        assert!(symbols.iter().any(|s| s.name == "greet" && s.kind == SymbolKind::Function));
        // Arrow function as const → upgraded to Function via post_process_kind
        assert!(
            symbols.iter().any(|s| s.name == "fadeIn" && s.kind == SymbolKind::Function),
            "arrow-as-const should be Function, got: {:?}",
            symbols.iter().find(|s| s.name == "fadeIn").map(|s| s.kind)
        );
        assert!(symbols.iter().any(|s| s.name == "cfg" && s.kind == SymbolKind::Variable));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Import));
    }

    #[test]
    fn test_query_typescript_method_has_class_parent() {
        let source = r#"
class Calculator {
    add(n: number): number { return n + 1; }
}
"#;
        let symbols = extract_via_query(Language::TypeScript, source);
        let add = symbols.iter().find(|s| s.name == "add").unwrap();
        assert_eq!(add.kind, SymbolKind::Method);
        assert_eq!(add.parent.as_deref(), Some("Calculator"));
    }

    #[test]
    fn test_query_javascript() {
        let source = r#"
class User { greet() { return "hi"; } }
function greet(name) { return name; }
const fadeIn = () => 1;
const cfg = { debug: true };
import { foo } from "./bar";
"#;
        let symbols = extract_via_query(Language::JavaScript, source);

        assert!(symbols.iter().any(|s| s.name == "User" && s.kind == SymbolKind::Class));
        assert!(symbols.iter().any(|s| s.name == "greet" && s.kind == SymbolKind::Method));
        assert!(symbols.iter().any(|s| s.name == "greet" && s.kind == SymbolKind::Function));
        assert!(
            symbols.iter().any(|s| s.name == "fadeIn" && s.kind == SymbolKind::Function),
            "arrow-as-const should be Function"
        );
        assert!(symbols.iter().any(|s| s.name == "cfg" && s.kind == SymbolKind::Variable));
    }

    #[test]
    fn test_query_python() {
        let source = r#"
import os
from foo import bar

class User:
    def __init__(self, name):
        self.name = name

    def greet(self):
        return self.name

def process(items):
    return items
"#;
        let symbols = extract_via_query(Language::Python, source);

        assert!(symbols.iter().any(|s| s.name == "User" && s.kind == SymbolKind::Class));
        assert!(symbols.iter().any(|s| s.name == "process" && s.kind == SymbolKind::Function));
        // __init__ et greet sont des méthodes (dans class body)
        let init = symbols.iter().find(|s| s.name == "__init__").unwrap();
        assert_eq!(init.kind, SymbolKind::Method);
        assert_eq!(init.parent.as_deref(), Some("User"));
        let greet = symbols.iter().find(|s| s.name == "greet").unwrap();
        assert_eq!(greet.kind, SymbolKind::Method);
        assert_eq!(greet.parent.as_deref(), Some("User"));
        // Imports
        assert!(symbols.iter().filter(|s| s.kind == SymbolKind::Import).count() >= 2);
    }

    #[test]
    fn test_query_python_decorated_function() {
        let source = r#"
class Service:
    @staticmethod
    def helper():
        pass
"#;
        let symbols = extract_via_query(Language::Python, source);
        let helper = symbols.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(helper.kind, SymbolKind::Method);
        assert_eq!(helper.parent.as_deref(), Some("Service"));
    }

    #[test]
    fn test_query_go() {
        let source = r#"
package main

import "fmt"

type User struct {
    Name string
}

type Greeter interface {
    Greet() string
}

func (u *User) Greet() string {
    return u.Name
}

func main() {
    fmt.Println("hi")
}

const Pi = 3.14
var counter = 0
"#;
        let symbols = extract_via_query(Language::Go, source);

        assert!(symbols.iter().any(|s| s.name == "User" && s.kind == SymbolKind::Struct));
        assert!(symbols.iter().any(|s| s.name == "Greeter" && s.kind == SymbolKind::Interface));
        assert!(symbols.iter().any(|s| s.name == "Greet" && s.kind == SymbolKind::Method));
        assert!(symbols.iter().any(|s| s.name == "main" && s.kind == SymbolKind::Function));
        assert!(symbols.iter().any(|s| s.name == "Pi" && s.kind == SymbolKind::Constant));
        assert!(symbols.iter().any(|s| s.name == "counter" && s.kind == SymbolKind::Variable));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Import));
    }

    #[test]
    fn test_query_java() {
        let source = r#"
import java.util.List;

public class Calculator {
    private int value;

    public Calculator(int v) {
        this.value = v;
    }

    public int add(int n) {
        return value + n;
    }
}

interface Computable {
    int compute();
}

enum Status { ACTIVE, INACTIVE }
"#;
        let symbols = extract_via_query(Language::Java, source);

        assert!(symbols.iter().any(|s| s.name == "Calculator" && s.kind == SymbolKind::Class));
        assert!(symbols.iter().any(|s| s.name == "Computable" && s.kind == SymbolKind::Interface));
        assert!(symbols.iter().any(|s| s.name == "Status" && s.kind == SymbolKind::Enum));
        let add = symbols.iter().find(|s| s.name == "add").unwrap();
        assert_eq!(add.kind, SymbolKind::Method);
        assert_eq!(add.parent.as_deref(), Some("Calculator"));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Import));
    }

    #[test]
    fn test_query_c() {
        let source = r#"
#include <stdio.h>

struct Point {
    int x;
    int y;
};

enum Color { RED, GREEN, BLUE };

int add(int a, int b) {
    return a + b;
}

int* make_buf() {
    return 0;
}
"#;
        let symbols = extract_via_query(Language::C, source);

        assert!(symbols.iter().any(|s| s.name == "Point" && s.kind == SymbolKind::Struct));
        assert!(symbols.iter().any(|s| s.name == "Color" && s.kind == SymbolKind::Enum));
        assert!(symbols.iter().any(|s| s.name == "add" && s.kind == SymbolKind::Function));
        assert!(
            symbols.iter().any(|s| s.name == "make_buf" && s.kind == SymbolKind::Function),
            "pointer_declarator wrapped function should still be Function, got: {:?}",
            symbols.iter().filter(|s| s.kind == SymbolKind::Function).collect::<Vec<_>>()
        );
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Import));
    }

    #[test]
    fn test_query_cpp() {
        let source = r#"
namespace ns {
class Calculator {
public:
    int add(int n);
};
}

int ns::Calculator::add(int n) {
    return n + 1;
}

struct Point { int x; int y; };
"#;
        let symbols = extract_via_query(Language::Cpp, source);

        assert!(symbols.iter().any(|s| s.name == "Calculator" && s.kind == SymbolKind::Class));
        assert!(symbols.iter().any(|s| s.name == "Point" && s.kind == SymbolKind::Struct));
        // Method definition outside class via qualified_identifier — name is captured as
        // the full "ns::Calculator::add" because tree-sitter-cpp models the qualifier
        // recursively and we keep the full text.
        assert!(
            symbols
                .iter()
                .any(|s| s.name.contains("add") && s.kind == SymbolKind::Method),
            "qualified Foo::bar method should be captured as Method, got: {:?}",
            symbols.iter().map(|s| (&s.name, s.kind)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_query_php() {
        let source = r#"<?php
namespace App\Service;

use Foo\Bar;

class UserService {
    const VERSION = "1.0";

    public function greet($name) {
        return $name;
    }
}

interface Greeter { public function greet(); }

trait Loggable { public function log() {} }

enum Status { case Active; case Inactive; }

function helper() { return 1; }
"#;
        let symbols = extract_via_query(Language::Php, source);

        assert!(symbols.iter().any(|s| s.name == "UserService" && s.kind == SymbolKind::Class));
        assert!(symbols.iter().any(|s| s.name == "Greeter" && s.kind == SymbolKind::Interface));
        assert!(symbols.iter().any(|s| s.name == "Loggable" && s.kind == SymbolKind::Trait));
        assert!(symbols.iter().any(|s| s.name == "Status" && s.kind == SymbolKind::Enum));
        assert!(symbols.iter().any(|s| s.name == "helper" && s.kind == SymbolKind::Function));
        assert!(symbols.iter().any(|s| s.name == "greet" && s.kind == SymbolKind::Method));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Module));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Import));
    }

    #[test]
    fn test_query_ruby() {
        let source = r#"
class User
  def initialize(name)
    @name = name
  end

  def greet
    @name
  end
end

module Utils
  def self.helper
    "hi"
  end
end
"#;
        let symbols = extract_via_query(Language::Ruby, source);

        assert!(symbols.iter().any(|s| s.name == "User" && s.kind == SymbolKind::Class));
        assert!(symbols.iter().any(|s| s.name == "Utils" && s.kind == SymbolKind::Module));
        // Legacy maps method → Function (not Method) — preserved
        assert!(
            symbols.iter().any(|s| s.name == "initialize" && s.kind == SymbolKind::Function),
            "Ruby `def` should map to Function, got: {:?}",
            symbols.iter().find(|s| s.name == "initialize").map(|s| s.kind)
        );
        assert!(symbols.iter().any(|s| s.name == "helper" && s.kind == SymbolKind::Function));
    }

    #[test]
    fn test_query_csharp() {
        let source = r#"
using System;

namespace MyApp {
    public class Calculator {
        private int value;

        public int Add(int n) {
            int Local() { return n; }
            return value + n;
        }
    }

    public struct Point { public int X; public int Y; }
    public interface IGreeter { string Greet(); }
    public enum Status { Active, Inactive }
}
"#;
        let symbols = extract_via_query(Language::CSharp, source);

        assert!(symbols.iter().any(|s| s.name == "MyApp" && s.kind == SymbolKind::Module));
        assert!(symbols.iter().any(|s| s.name == "Calculator" && s.kind == SymbolKind::Class));
        assert!(symbols.iter().any(|s| s.name == "Point" && s.kind == SymbolKind::Struct));
        assert!(symbols.iter().any(|s| s.name == "IGreeter" && s.kind == SymbolKind::Interface));
        assert!(symbols.iter().any(|s| s.name == "Status" && s.kind == SymbolKind::Enum));
        assert!(symbols.iter().any(|s| s.name == "Add" && s.kind == SymbolKind::Method));
        assert!(symbols.iter().any(|s| s.name == "Local" && s.kind == SymbolKind::Function));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Import));
    }

    #[test]
    fn test_query_kotlin() {
        // tree-sitter-kotlin-ng exige les imports juste après package_header (sinon
        // ils parsent en navigation_expression). On utilise un fichier complet pour
        // valider également interface, enum class et imports.
        let source = r#"package com.example
import kotlin.io.println

interface Greeter { fun greet(): String }

enum class Status { ACTIVE, INACTIVE }

class User(val name: String) {
    fun greet(): String = "Hello $name"
    val nested: String = "x"
}

object Singleton {
    fun helper() = 1
}

fun main() {
    println("hi")
}
"#;
        let symbols = extract_via_query(Language::Kotlin, source);

        // Class / Interface / Enum / Object — précision préservée
        assert!(symbols.iter().any(|s| s.name == "User" && s.kind == SymbolKind::Class));
        assert!(symbols.iter().any(|s| s.name == "Singleton" && s.kind == SymbolKind::Class));
        assert!(
            symbols.iter().any(|s| s.name == "Greeter" && s.kind == SymbolKind::Interface),
            "Greeter must be Interface, got: {:?}",
            symbols.iter().find(|s| s.name == "Greeter").map(|s| s.kind)
        );
        assert!(
            symbols.iter().any(|s| s.name == "Status" && s.kind == SymbolKind::Enum),
            "enum class Status must be Enum, got: {:?}",
            symbols.iter().find(|s| s.name == "Status").map(|s| s.kind)
        );

        // Function vs Method — fonctions dans class_body sont Method
        assert!(symbols.iter().any(|s| s.name == "main" && s.kind == SymbolKind::Function));
        let user_greet = symbols.iter().find(|s| s.name == "greet" && s.parent.as_deref() == Some("User")).unwrap();
        assert_eq!(user_greet.kind, SymbolKind::Method);
        let helper = symbols.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(helper.kind, SymbolKind::Method);
        assert_eq!(helper.parent.as_deref(), Some("Singleton"));

        // Property avec parent
        let nested = symbols.iter().find(|s| s.name == "nested").unwrap();
        assert_eq!(nested.kind, SymbolKind::Variable);
        assert_eq!(nested.parent.as_deref(), Some("User"));

        // Import — capturé via le pattern `(import (qualified_identifier) @name)`
        assert!(
            symbols.iter().any(|s| s.kind == SymbolKind::Import),
            "kotlin import après package_header doit être capturé"
        );
    }

    #[test]
    fn test_query_scala() {
        let source = r#"
import scala.collection.mutable

class Calculator(val initial: Int) {
    def add(n: Int): Int = initial + n
}

object Helpers {
    def util(): Int = 1
}

trait Greeter {
    def greet(): String
}

val PI = 3.14
"#;
        let symbols = extract_via_query(Language::Scala, source);

        assert!(symbols.iter().any(|s| s.name == "Calculator" && s.kind == SymbolKind::Class));
        assert!(symbols.iter().any(|s| s.name == "Helpers" && s.kind == SymbolKind::Class));
        assert!(symbols.iter().any(|s| s.name == "Greeter" && s.kind == SymbolKind::Trait));
        assert!(symbols.iter().any(|s| s.name == "add" && s.kind == SymbolKind::Function));
        assert!(symbols.iter().any(|s| s.name == "PI" && s.kind == SymbolKind::Variable));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Import));
    }

    #[test]
    fn test_query_bash() {
        let source = r#"
#!/bin/bash
NAME="world"
PORT=8080

greet() {
    echo "hello $1"
}

function helper {
    echo "helper"
}
"#;
        let symbols = extract_via_query(Language::Bash, source);

        assert!(symbols.iter().any(|s| s.name == "NAME" && s.kind == SymbolKind::Variable));
        assert!(symbols.iter().any(|s| s.name == "PORT" && s.kind == SymbolKind::Variable));
        assert!(symbols.iter().any(|s| s.name == "greet" && s.kind == SymbolKind::Function));
        assert!(symbols.iter().any(|s| s.name == "helper" && s.kind == SymbolKind::Function));
    }

    #[test]
    fn test_query_elixir() {
        let source = r#"
defmodule MyApp.User do
  def hello(name) do
    "hi"
  end

  defp internal_helper do
    1
  end
end
"#;
        let symbols = extract_via_query(Language::Elixir, source);

        // defmodule → Module (precision improvement over legacy)
        assert!(
            symbols.iter().any(|s| s.kind == SymbolKind::Module),
            "defmodule should produce a Module symbol, got: {:?}",
            symbols.iter().map(|s| (&s.name, s.kind)).collect::<Vec<_>>()
        );
        // def hello → Function
        assert!(
            symbols.iter().any(|s| s.name == "hello" && s.kind == SymbolKind::Function),
            "def should produce Function 'hello', got: {:?}",
            symbols.iter().map(|s| (&s.name, s.kind)).collect::<Vec<_>>()
        );
        // defp internal_helper → Function
        assert!(
            symbols.iter().any(|s| s.name == "internal_helper" && s.kind == SymbolKind::Function),
            "defp should produce Function 'internal_helper'"
        );
    }

    #[test]
    fn test_query_html() {
        // Sémantique top-level only : seuls les enfants directs de `document`
        // sont capturés. Pour ce snippet, `<html>` est top-level mais les
        // <script>/<style> imbriqués dans <head> ne le sont pas.
        let source = r#"<!DOCTYPE html>
<html>
<head>
    <script>console.log("x");</script>
    <style>body { color: red; }</style>
</head>
<body>
    <div>Hello</div>
</body>
</html>"#;
        let symbols = extract_via_query(Language::Html, source);

        // Seulement <html> est enfant direct de document → 1 symbole.
        assert_eq!(symbols.len(), 1, "expected only top-level <html>, got: {:?}",
            symbols.iter().map(|s| &s.name).collect::<Vec<_>>());
        let html = &symbols[0];
        assert_eq!(html.name, "html");
        assert_eq!(html.kind, SymbolKind::Variable);
    }

    #[test]
    fn test_query_html_top_level_script_style() {
        // Snippet où <script> et <style> sont eux-mêmes top-level (fragment HTML).
        let source = r#"<script>console.log("x")</script>
<style>body { color: red; }</style>"#;
        let symbols = extract_via_query(Language::Html, source);
        assert!(
            symbols.iter().any(|s| s.name == "script" && s.kind == SymbolKind::Module),
            "top-level script must be Module"
        );
        assert!(
            symbols.iter().any(|s| s.name == "style" && s.kind == SymbolKind::Module),
            "top-level style must be Module"
        );
    }

    #[test]
    fn test_query_json() {
        let source = r#"{
    "name": "semantiq",
    "version": "0.6.2",
    "nested": {
        "key": "value"
    }
}"#;
        let symbols = extract_via_query(Language::Json, source);

        // 4 paires : name, version, nested (top), key (nested)
        assert_eq!(symbols.len(), 4, "expected 4 keys, got: {:?}",
            symbols.iter().map(|s| &s.name).collect::<Vec<_>>());

        // Top-level keys : pas de parent
        let name = symbols.iter().find(|s| s.name == "name").unwrap();
        assert_eq!(name.kind, SymbolKind::Variable);
        assert_eq!(name.parent, None);
        assert_eq!(symbols.iter().find(|s| s.name == "version").unwrap().parent, None);
        assert_eq!(symbols.iter().find(|s| s.name == "nested").unwrap().parent, None);

        // Clé imbriquée : parent dot-separated
        let key = symbols.iter().find(|s| s.name == "key").unwrap();
        assert_eq!(key.parent.as_deref(), Some("nested"));
    }

    #[test]
    fn test_query_yaml() {
        let source = r#"
name: semantiq
version: 0.6.2
nested:
  key: value
"#;
        let symbols = extract_via_query(Language::Yaml, source);

        // 4 paires
        assert_eq!(symbols.len(), 4);
        let name = symbols.iter().find(|s| s.name == "name").unwrap();
        assert_eq!(name.kind, SymbolKind::Variable);
        assert_eq!(name.parent, None);
        let key = symbols.iter().find(|s| s.name == "key").unwrap();
        assert_eq!(key.parent.as_deref(), Some("nested"));
    }

    #[test]
    fn test_query_toml() {
        let source = r#"
name = "semantiq"
version = "0.6.2"

[package]
edition = "2024"
"#;
        let symbols = extract_via_query(Language::Toml, source);

        // 2 pairs top-level + 1 table + 1 pair imbriqué = 4 symbols
        assert_eq!(symbols.len(), 4, "got: {:?}",
            symbols.iter().map(|s| (&s.name, s.kind, &s.parent)).collect::<Vec<_>>());

        let name = symbols.iter().find(|s| s.name == "name").unwrap();
        assert_eq!(name.kind, SymbolKind::Variable);
        assert_eq!(name.parent, None);

        // [package] est Struct, top-level
        let package = symbols.iter().find(|s| s.name == "package").unwrap();
        assert_eq!(package.kind, SymbolKind::Struct);

        // edition est dans [package]
        let edition = symbols.iter().find(|s| s.name == "edition").unwrap();
        assert_eq!(edition.kind, SymbolKind::Variable);
        assert_eq!(edition.parent.as_deref(), Some("package"));
    }

    // -------------------------------------------------------------------------
    // Parity tests : pour chaque langage migré, on charge une fixture
    // représentative et on compare l'extraction query-based vs legacy.
    // Les écarts attendus (corrections délibérées : Kotlin Interface vs Class,
    // C++ inline méthodes capturées, Elixir defmodule = Module, séparateur
    // dot-separated, imports nom court, HTML top-level only, etc.) sont
    // documentés via une closure `expected_diff` par langage.
    // -------------------------------------------------------------------------

    /// Diff sur l'ensemble des `(name, kind)` extraits — bypass des `parent` et
    /// ranges car ils peuvent différer légitimement entre les deux extracteurs.
    fn name_kind_set(
        symbols: &[crate::symbols::Symbol],
    ) -> std::collections::HashSet<(String, SymbolKind)> {
        symbols.iter().map(|s| (s.name.clone(), s.kind)).collect()
    }

    fn parity_check(lang: Language, source: &str) -> ParityReport {
        let mut support = LanguageSupport::new().unwrap();
        let tree = support.parse(lang, source).unwrap();
        let query = QuerySymbolExtractor::new()
            .unwrap()
            .extract(&tree, source, lang)
            .unwrap();
        let legacy = SymbolExtractor::extract_legacy(&tree, source, lang).unwrap();
        let q_set = name_kind_set(&query);
        let l_set = name_kind_set(&legacy);
        let query_only: Vec<(String, SymbolKind)> =
            q_set.difference(&l_set).cloned().collect();
        let legacy_only: Vec<(String, SymbolKind)> =
            l_set.difference(&q_set).cloned().collect();
        ParityReport {
            query_only,
            legacy_only,
            query,
            legacy,
        }
    }

    struct ParityReport {
        query_only: Vec<(String, SymbolKind)>,
        legacy_only: Vec<(String, SymbolKind)>,
        query: Vec<crate::symbols::Symbol>,
        legacy: Vec<crate::symbols::Symbol>,
    }

    impl ParityReport {
        fn print(&self, label: &str) {
            eprintln!(
                "\n[{label}] query={} legacy={}\n  query_only: {:?}\n  legacy_only: {:?}",
                self.query.len(),
                self.legacy.len(),
                self.query_only,
                self.legacy_only,
            );
        }
    }

    #[test]
    fn test_query_vs_legacy_typescript() {
        let src = include_str!("../tests/fixtures/typescript/sample.ts");
        let r = parity_check(Language::TypeScript, src);
        r.print("typescript");
        // Query gagne en précision : addUser devient Method (legacy ne capture pas
        // les méthodes individuelles dans cette grammaire).
        assert!(r.query.iter().any(|s| s.name == "User" && s.kind == SymbolKind::Interface));
        assert!(r.query.iter().any(|s| s.name == "UserService" && s.kind == SymbolKind::Class));
        assert!(r.query.iter().any(|s| s.name == "addUser" && s.kind == SymbolKind::Method));
        // Arrow-as-const → Function (post-process)
        assert!(r.query.iter().any(|s| s.name == "fadeIn" && s.kind == SymbolKind::Function));
        assert!(r.query.iter().any(|s| s.name == "config" && s.kind == SymbolKind::Variable));
    }

    #[test]
    fn test_query_vs_legacy_javascript() {
        let src = include_str!("../tests/fixtures/javascript/sample.js");
        let r = parity_check(Language::JavaScript, src);
        r.print("javascript");
        assert!(r.query.iter().any(|s| s.name == "Calculator" && s.kind == SymbolKind::Class));
        assert!(r.query.iter().any(|s| s.name == "add" && s.kind == SymbolKind::Method));
        assert!(r.query.iter().any(|s| s.name == "multiply" && s.kind == SymbolKind::Function));
        assert!(r.query.iter().any(|s| s.name == "settings" && s.kind == SymbolKind::Variable));
    }

    #[test]
    fn test_query_vs_legacy_python() {
        let src = include_str!("../tests/fixtures/python/sample.py");
        let r = parity_check(Language::Python, src);
        r.print("python");
        // Méthode décorée NON doublonnée
        let from_dict = r.query.iter().filter(|s| s.name == "from_dict").count();
        assert_eq!(from_dict, 1, "decorated method must not be duplicated");
        let g = r.query.iter().find(|s| s.name == "from_dict").unwrap();
        assert_eq!(g.kind, SymbolKind::Method);
        assert_eq!(g.parent.as_deref(), Some("User"));
        // Imports nom court
        assert!(r.query.iter().any(|s| s.name == "os" && s.kind == SymbolKind::Import));
    }

    #[test]
    fn test_query_vs_legacy_go() {
        let src = include_str!("../tests/fixtures/go/sample.go");
        let r = parity_check(Language::Go, src);
        r.print("go");
        assert!(r.query.iter().any(|s| s.name == "User" && s.kind == SymbolKind::Struct));
        assert!(r.query.iter().any(|s| s.name == "Greeter" && s.kind == SymbolKind::Interface));
        assert!(r.query.iter().any(|s| s.name == "Greet" && s.kind == SymbolKind::Method));
        assert!(r.query.iter().any(|s| s.name == "main" && s.kind == SymbolKind::Function));
    }

    #[test]
    fn test_query_vs_legacy_java() {
        let src = include_str!("../tests/fixtures/java/sample.java");
        let r = parity_check(Language::Java, src);
        r.print("java");
        assert!(r.query.iter().any(|s| s.name == "Calculator" && s.kind == SymbolKind::Class));
        assert!(r.query.iter().any(|s| s.name == "Computable" && s.kind == SymbolKind::Interface));
        assert!(r.query.iter().any(|s| s.name == "Status" && s.kind == SymbolKind::Enum));
        let add = r.query.iter().find(|s| s.name == "add").unwrap();
        assert_eq!(add.kind, SymbolKind::Method);
        assert_eq!(add.parent.as_deref(), Some("Calculator"));
    }

    #[test]
    fn test_query_vs_legacy_c() {
        let src = include_str!("../tests/fixtures/c/sample.c");
        let r = parity_check(Language::C, src);
        r.print("c");
        assert!(r.query.iter().any(|s| s.name == "Point" && s.kind == SymbolKind::Struct));
        assert!(r.query.iter().any(|s| s.name == "Color" && s.kind == SymbolKind::Enum));
        assert!(r.query.iter().any(|s| s.name == "add" && s.kind == SymbolKind::Function));
        assert!(r.query.iter().any(|s| s.name == "make_buf" && s.kind == SymbolKind::Function));
    }

    #[test]
    fn test_query_vs_legacy_cpp() {
        let src = include_str!("../tests/fixtures/cpp/sample.cpp");
        let r = parity_check(Language::Cpp, src);
        r.print("cpp");
        // C++ : méthodes inline désormais capturées (legacy les rate)
        let add = r.query.iter().find(|s| s.name == "add").expect("inline `add` missing");
        assert_eq!(add.kind, SymbolKind::Method);
        assert_eq!(add.parent.as_deref(), Some("ns::Calculator"));
        assert!(r.query.iter().any(|s| s.name == "~Calculator" && s.kind == SymbolKind::Method));
        assert!(r.query.iter().any(|s| s.name == "Calculator" && s.kind == SymbolKind::Class));
    }

    #[test]
    fn test_query_vs_legacy_php() {
        let src = include_str!("../tests/fixtures/php/sample.php");
        let r = parity_check(Language::Php, src);
        r.print("php");
        assert!(r.query.iter().any(|s| s.name == "UserService" && s.kind == SymbolKind::Class));
        assert!(r.query.iter().any(|s| s.name == "Greeter" && s.kind == SymbolKind::Interface));
        assert!(r.query.iter().any(|s| s.name == "Loggable" && s.kind == SymbolKind::Trait));
        assert!(r.query.iter().any(|s| s.name == "Status" && s.kind == SymbolKind::Enum));
        // Import nom court (Bar pas "use Foo\Bar;")
        let imp = r.query.iter().find(|s| s.kind == SymbolKind::Import).unwrap();
        assert_eq!(imp.name, "Bar");
    }

    #[test]
    fn test_query_vs_legacy_ruby() {
        let src = include_str!("../tests/fixtures/ruby/sample.rb");
        let r = parity_check(Language::Ruby, src);
        r.print("ruby");
        assert!(r.query.iter().any(|s| s.name == "User" && s.kind == SymbolKind::Class));
        assert!(r.query.iter().any(|s| s.name == "Utils" && s.kind == SymbolKind::Module));
        // Ruby legacy mappe def → Function (préservé)
        assert!(r.query.iter().any(|s| s.name == "initialize" && s.kind == SymbolKind::Function));
    }

    #[test]
    fn test_query_vs_legacy_csharp() {
        let src = include_str!("../tests/fixtures/csharp/sample.cs");
        let r = parity_check(Language::CSharp, src);
        r.print("csharp");
        assert!(r.query.iter().any(|s| s.name == "UserService" && s.kind == SymbolKind::Class));
        assert!(r.query.iter().any(|s| s.name == "User" && s.kind == SymbolKind::Struct));
        assert!(r.query.iter().any(|s| s.name == "IGreeter" && s.kind == SymbolKind::Interface));
        assert!(r.query.iter().any(|s| s.name == "AddUser" && s.kind == SymbolKind::Method));
    }

    #[test]
    fn test_query_vs_legacy_kotlin() {
        let src = include_str!("../tests/fixtures/kotlin/sample.kt");
        let r = parity_check(Language::Kotlin, src);
        r.print("kotlin");
        // Précisions Kotlin que la query apporte :
        assert!(
            r.query.iter().any(|s| s.name == "Greeter" && s.kind == SymbolKind::Interface),
            "Kotlin Greeter must be Interface (legacy: Class)",
        );
        assert!(
            r.query.iter().any(|s| s.name == "Status" && s.kind == SymbolKind::Enum),
            "Kotlin enum class Status must be Enum (legacy: Class)",
        );
        // Méthodes dans class_body → Method (legacy : Function)
        let user_greet = r
            .query
            .iter()
            .find(|s| s.name == "greet" && s.parent.as_deref() == Some("User"))
            .unwrap();
        assert_eq!(user_greet.kind, SymbolKind::Method);
        // Import capturé
        assert!(r.query.iter().any(|s| s.kind == SymbolKind::Import));
    }

    #[test]
    fn test_query_vs_legacy_scala() {
        let src = include_str!("../tests/fixtures/scala/sample.scala");
        let r = parity_check(Language::Scala, src);
        r.print("scala");
        assert!(r.query.iter().any(|s| s.name == "Calculator" && s.kind == SymbolKind::Class));
        assert!(r.query.iter().any(|s| s.name == "Helpers" && s.kind == SymbolKind::Class));
        assert!(r.query.iter().any(|s| s.name == "Greeter" && s.kind == SymbolKind::Trait));
        assert!(r.query.iter().any(|s| s.name == "PI" && s.kind == SymbolKind::Variable));
    }

    #[test]
    fn test_query_vs_legacy_html() {
        let src = include_str!("../tests/fixtures/html/sample.html");
        let r = parity_check(Language::Html, src);
        r.print("html");
        // HTML : query top-level only → quelques symboles seulement (pas tous les divs).
        assert!(
            r.query.len() <= 3,
            "HTML must capture only top-level (≤ 3 symbols), got {}: {:?}",
            r.query.len(),
            r.query.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        assert!(r.query.iter().any(|s| s.name == "html"));
    }

    #[test]
    fn test_query_vs_legacy_json() {
        let src = include_str!("../tests/fixtures/json/sample.json");
        let r = parity_check(Language::Json, src);
        r.print("json");
        // Top-level keys : pas de parent
        assert!(r.query.iter().any(|s| s.name == "name" && s.parent.is_none()));
        // Clés imbriquées : parent dot-separated
        assert!(
            r.query.iter().any(|s| s.name == "build" && s.parent.as_deref() == Some("scripts")),
            "json key 'build' must have parent='scripts', got: {:?}",
            r.query.iter().find(|s| s.name == "build").map(|s| &s.parent)
        );
        assert!(r.query.iter().any(|s| s.name == "react" && s.parent.as_deref() == Some("deps")));
    }

    #[test]
    fn test_query_vs_legacy_yaml() {
        let src = include_str!("../tests/fixtures/yaml/sample.yaml");
        let r = parity_check(Language::Yaml, src);
        r.print("yaml");
        assert!(r.query.iter().any(|s| s.name == "name" && s.parent.is_none()));
        assert!(
            r.query.iter().any(|s| s.name == "host" && s.parent.as_deref() == Some("server")),
            "yaml 'host' must have parent='server'"
        );
    }

    #[test]
    fn test_query_vs_legacy_toml() {
        let src = include_str!("../tests/fixtures/toml/sample.toml");
        let r = parity_check(Language::Toml, src);
        r.print("toml");
        assert!(r.query.iter().any(|s| s.name == "server" && s.kind == SymbolKind::Struct));
        assert!(
            r.query.iter().any(|s| s.name == "host" && s.parent.as_deref() == Some("server")),
            "toml 'host' must have parent='server'"
        );
        // [server.deep] → table dotted_key
        assert!(r.query.iter().any(|s| s.name == "server.deep" && s.kind == SymbolKind::Struct));
    }

    #[test]
    fn test_query_vs_legacy_bash() {
        let src = include_str!("../tests/fixtures/bash/sample.sh");
        let r = parity_check(Language::Bash, src);
        r.print("bash");
        assert!(r.query.iter().any(|s| s.name == "NAME" && s.kind == SymbolKind::Variable));
        assert!(r.query.iter().any(|s| s.name == "greet" && s.kind == SymbolKind::Function));
        assert!(r.query.iter().any(|s| s.name == "helper" && s.kind == SymbolKind::Function));
    }

    #[test]
    fn test_query_vs_legacy_elixir() {
        let src = include_str!("../tests/fixtures/elixir/sample.ex");
        let r = parity_check(Language::Elixir, src);
        r.print("elixir");
        // defmodule → Module (légère amélioration sur le legacy qui mappait à Function)
        assert!(
            r.query.iter().any(|s| s.name == "MyApp.User" && s.kind == SymbolKind::Module),
            "defmodule MyApp.User must be Module"
        );
        // defmacro capturé
        assert!(r.query.iter().any(|s| s.name == "guarded"));
        // Parent dot-separated pour modules imbriqués
        assert!(
            r.query.iter().any(|s| s.name == "Inner" && s.parent.as_deref() == Some("MyApp.Outer")),
            "Inner must have parent='MyApp.Outer' (dot separator)"
        );
        let deep = r.query.iter().find(|s| s.name == "deep").unwrap();
        assert_eq!(deep.parent.as_deref(), Some("MyApp.Outer.Inner"));
    }

    #[test]
    fn test_query_vs_legacy_rust_fixture() {
        // Pendant Rust de la suite parity (le test_query_vs_legacy_rust historique
        // utilise un snippet inline ; on ajoute ici le test parametré sur fixture).
        let src = include_str!("../tests/fixtures/rust/sample.rs");
        let r = parity_check(Language::Rust, src);
        r.print("rust");
        assert!(r.query.iter().any(|s| s.name == "User" && s.kind == SymbolKind::Struct));
        assert!(r.query.iter().any(|s| s.name == "Greetable" && s.kind == SymbolKind::Trait));
        assert!(r.query.iter().any(|s| s.name == "Status" && s.kind == SymbolKind::Enum));
        // impl_item NE doit PAS apparaître comme Class
        assert!(!r.query.iter().any(|s| s.kind == SymbolKind::Class));
        // greet est Method dans impl Greetable for User
        let greet = r.query.iter().find(|s| s.name == "greet").unwrap();
        assert_eq!(greet.kind, SymbolKind::Method);
        // Import nom court
        assert!(r.query.iter().any(|s| s.name == "HashMap" && s.kind == SymbolKind::Import));
    }
}
