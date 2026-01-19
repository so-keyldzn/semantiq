pub mod auto_indexer;
pub mod exclusions;
pub mod schema;
pub mod store;
pub mod watcher;

pub use auto_indexer::{AutoIndexer, InitialIndexResult, ProcessResult};
pub use exclusions::{
    EXCLUDED_DIRS, MAX_FILE_SIZE, should_exclude, should_exclude_entry, should_exclude_path,
};
pub use schema::{ChunkRecord, DependencyRecord, FileRecord, SymbolRecord};
pub use store::IndexStore;
pub use watcher::FileWatcher;
