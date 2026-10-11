pub mod chunks;
pub mod imports;
pub mod language;
mod python_stdlib;
pub mod query_extractor;
pub mod references;
pub mod resolve;
pub mod structure;
pub mod symbols;

/// Version du parser. Incrémenter force une réindexation complète.
/// Incrémenter quand : ajout/modif de types de noeuds, changement logique d'extraction
pub const PARSER_VERSION: u32 = 14; // v14: Elixir `def*` heads are definitions (keywords not indexed, directive arguments are imports), Rust calls inside macro arguments are `call` references, identifier strings in attribute arguments (`#[serde(default = "f")]`) are references, doc comments found through `export` / decorator wrappers and above Rust attributes, oversized symbol spans split into windowed chunks. v13: Local imports resolved on the full file name (`./user.service` -> `user.service.ts`), explicit `.js`/`.jsx`/`.mjs` -> `.ts`/`.tsx` mapping, bundler query suffixes (`?raw`) ignored. v12: Occurrence count on references and call edges, import lines on dependencies. v11: Call edges (caller of each call) and type relations (extends/implements). v10: AST references (identifier occurrences) extracted for find_refs. v9: Rust brace imports expanded into one import per leaf. v8: Multi-declarator dedup (name range in key), Scala multi-binding val/var, doc-comment blank-line break

pub use chunks::{ChunkExtractor, CodeChunk};
pub use imports::{Import, ImportExtractor, ImportKind};
pub use language::{Language, LanguageSupport};
pub use query_extractor::QuerySymbolExtractor;
pub use references::{Reference, ReferenceExtractor, ReferenceKind};
pub use resolve::resolve_local_import;
pub use structure::{CallEdge, FileStructure, RelationKind, StructureExtractor, TypeRelation};
pub use symbols::{Symbol, SymbolExtractor, SymbolKind};
