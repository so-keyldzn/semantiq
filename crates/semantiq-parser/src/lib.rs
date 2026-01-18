pub mod language;
pub mod symbols;
pub mod chunks;
pub mod imports;

/// Version du parser. Incrémenter force une réindexation complète.
/// Incrémenter quand : ajout/modif de types de noeuds, changement logique d'extraction
pub const PARSER_VERSION: u32 = 2; // Start at 2 to force reindex for existing DBs

pub use language::{Language, LanguageSupport};
pub use symbols::{Symbol, SymbolKind, SymbolExtractor};
pub use chunks::{CodeChunk, ChunkExtractor};
pub use imports::{Import, ImportKind, ImportExtractor};
