pub mod chunks;
pub mod imports;
pub mod language;
pub mod symbols;

/// Version du parser. Incrémenter force une réindexation complète.
/// Incrémenter quand : ajout/modif de types de noeuds, changement logique d'extraction
pub const PARSER_VERSION: u32 = 2; // Start at 2 to force reindex for existing DBs

pub use chunks::{ChunkExtractor, CodeChunk};
pub use imports::{Import, ImportExtractor, ImportKind};
pub use language::{Language, LanguageSupport};
pub use symbols::{Symbol, SymbolExtractor, SymbolKind};
