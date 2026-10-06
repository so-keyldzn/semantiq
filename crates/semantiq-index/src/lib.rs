pub mod auto_indexer;
pub mod exclusions;
pub mod paths;
pub mod schema;
pub mod store;
pub mod watcher;

pub use auto_indexer::{AutoIndexer, InitialIndexResult, ProcessResult};
pub use exclusions::{
    EXCLUDED_DIRS, MAX_DATA_FILE_SIZE, MAX_FILE_SIZE, exceeds_indexed_size, max_indexed_size,
    should_exclude, should_exclude_entry, should_exclude_path,
};
pub use schema::{
    CallEdgeRecord, ChunkRecord, DependencyRecord, FileRecord, ReferenceRecord, SymbolRecord,
    TypeRelationRecord, UnreferencedSymbol,
};
pub use store::{CalibrationData, CalibrationRecord, IndexStats, IndexStore};
pub use store::{GraphFile, GraphRefCount, RepoGraphData};
pub use watcher::FileWatcher;
