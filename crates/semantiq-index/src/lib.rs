pub mod auto_indexer;
pub mod embedder;
pub mod exclusions;
pub mod paths;
pub mod schema;
pub mod store;
pub mod watcher;

pub use auto_indexer::{AutoIndexer, InitialIndexResult, ProcessResult, read_error_is_final};
pub use embedder::{
    BackgroundEmbedder, EmbedResult, EmbedderConfig, EmbeddingProgress, ModelLoader, embed_pending,
};
pub use exclusions::{
    EXCLUDED_DIRS, MAX_DATA_FILE_SIZE, MAX_FILE_SIZE, WalkFailures, exceeds_indexed_size,
    max_indexed_size, project_walker, project_walker_with, should_exclude,
    should_exclude_dir_entry, should_exclude_entry, should_exclude_path, walk_error_path,
};
pub use schema::{
    CallEdgeRecord, ChunkRecord, DependencyRecord, FileRecord, ReferenceRecord, SymbolRecord,
    TypeRelationRecord, UnreferencedSymbol,
};
pub use store::{
    CalibrationData, CalibrationRecord, ChunkHit, ChunkTextHit, DependencyInsert, EmbeddingCounts,
    ExtensionFilter, FileIndexWrite, FileStamp, FileStampRefresh, IndexStats, IndexStore,
    PendingChunk,
};
pub use store::{GraphFile, GraphRefCount, MAX_NAME_DEFINERS, RepoGraphData};
pub use watcher::FileWatcher;
