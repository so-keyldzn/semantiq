pub mod chunks;
pub mod imports;
pub mod language;
mod python_stdlib;
pub mod resolve;
pub mod symbols;

/// Version du parser. Incrémenter force une réindexation complète.
/// Incrémenter quand : ajout/modif de types de noeuds, changement logique d'extraction
pub const PARSER_VERSION: u32 = 4; // Hierarchical parent paths (Outer::Middle::Inner)

pub use chunks::{ChunkExtractor, CodeChunk};
pub use imports::{Import, ImportExtractor, ImportKind};
pub use language::{Language, LanguageSupport};
pub use resolve::resolve_local_import;
pub use symbols::{Symbol, SymbolExtractor, SymbolKind};
