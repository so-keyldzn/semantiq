pub mod auto_indexer;
pub mod exclusions;
pub mod schema;
pub mod store;
pub mod watcher;

pub use auto_indexer::AutoIndexer;
pub use exclusions::{should_exclude, should_exclude_path, should_exclude_entry, MAX_FILE_SIZE, EXCLUDED_DIRS};
pub use schema::{FileRecord, SymbolRecord, ChunkRecord, DependencyRecord};
pub use store::IndexStore;
pub use watcher::FileWatcher;
