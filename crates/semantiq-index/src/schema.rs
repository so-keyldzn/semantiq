use rusqlite::{Connection, Result as SqliteResult};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: i32 = 2;

/// Embedding dimension (MiniLM-L6-v2 produces 384-dimensional vectors)
pub const EMBEDDING_DIMENSION: usize = 384;

pub fn init_schema(conn: &Connection) -> SqliteResult<()> {
    conn.execute_batch(
        r#"
        -- Metadata table for schema versioning
        CREATE TABLE IF NOT EXISTS metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        -- Files table
        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL UNIQUE,
            language TEXT,
            hash TEXT NOT NULL,
            size INTEGER NOT NULL,
            last_modified INTEGER NOT NULL,
            indexed_at INTEGER NOT NULL
        );

        -- Symbols table
        CREATE TABLE IF NOT EXISTS symbols (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            start_byte INTEGER NOT NULL,
            end_byte INTEGER NOT NULL,
            signature TEXT,
            doc_comment TEXT,
            parent TEXT,
            FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
        );

        -- Chunks table for semantic search
        CREATE TABLE IF NOT EXISTS chunks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            content TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            start_byte INTEGER NOT NULL,
            end_byte INTEGER NOT NULL,
            symbols_json TEXT,
            embedding BLOB,
            FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
        );

        -- Dependencies table
        CREATE TABLE IF NOT EXISTS dependencies (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_file_id INTEGER NOT NULL,
            target_path TEXT NOT NULL,
            import_name TEXT,
            kind TEXT NOT NULL,
            FOREIGN KEY (source_file_id) REFERENCES files(id) ON DELETE CASCADE
        );

        -- Indexes for performance
        CREATE INDEX IF NOT EXISTS idx_files_path ON files(path);
        CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
        CREATE INDEX IF NOT EXISTS idx_symbols_kind ON symbols(kind);
        CREATE INDEX IF NOT EXISTS idx_symbols_file_id ON symbols(file_id);
        CREATE INDEX IF NOT EXISTS idx_chunks_file_id ON chunks(file_id);
        CREATE INDEX IF NOT EXISTS idx_deps_source ON dependencies(source_file_id);
        CREATE INDEX IF NOT EXISTS idx_deps_target ON dependencies(target_path);

        -- FTS5 for full-text search on symbols
        CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
            name,
            signature,
            doc_comment,
            content='symbols',
            content_rowid='id'
        );

        -- Triggers to keep FTS in sync
        CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
            INSERT INTO symbols_fts(rowid, name, signature, doc_comment)
            VALUES (new.id, new.name, new.signature, new.doc_comment);
        END;

        CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
            INSERT INTO symbols_fts(symbols_fts, rowid, name, signature, doc_comment)
            VALUES ('delete', old.id, old.name, old.signature, old.doc_comment);
        END;

        CREATE TRIGGER IF NOT EXISTS symbols_au AFTER UPDATE ON symbols BEGIN
            INSERT INTO symbols_fts(symbols_fts, rowid, name, signature, doc_comment)
            VALUES ('delete', old.id, old.name, old.signature, old.doc_comment);
            INSERT INTO symbols_fts(rowid, name, signature, doc_comment)
            VALUES (new.id, new.name, new.signature, new.doc_comment);
        END;
        "#,
    )?;

    // Create sqlite-vec virtual table for vector similarity search
    // This table stores chunk embeddings for semantic search
    conn.execute_batch(&format!(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks_vec USING vec0(
            chunk_id INTEGER PRIMARY KEY,
            embedding float[{EMBEDDING_DIMENSION}]
        );
        "#
    ))?;

    // Set schema version
    conn.execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES ('schema_version', ?1)",
        [SCHEMA_VERSION.to_string()],
    )?;

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: i64,
    pub path: String,
    pub language: Option<String>,
    pub hash: String,
    pub size: i64,
    pub last_modified: i64,
    pub indexed_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolRecord {
    pub id: i64,
    pub file_id: i64,
    pub name: String,
    pub kind: String,
    pub start_line: i64,
    pub end_line: i64,
    pub start_byte: i64,
    pub end_byte: i64,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRecord {
    pub id: i64,
    pub file_id: i64,
    pub content: String,
    pub start_line: i64,
    pub end_line: i64,
    pub start_byte: i64,
    pub end_byte: i64,
    pub symbols: Vec<String>,
    pub embedding: Option<Vec<f32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyRecord {
    pub id: i64,
    pub source_file_id: i64,
    pub target_path: String,
    pub import_name: Option<String>,
    pub kind: String,
}

#[cfg(test)]
mod tests {
    use crate::IndexStore;

    #[test]
    fn test_init_schema() {
        // Use IndexStore::open_in_memory() which properly initializes sqlite-vec
        // before creating the database connection.
        let store = IndexStore::open_in_memory().unwrap();

        // Verify tables exist by getting stats (which queries the tables)
        let stats = store.get_stats().unwrap();
        assert_eq!(stats.file_count, 0);
        assert_eq!(stats.symbol_count, 0);
        assert_eq!(stats.chunk_count, 0);
        assert_eq!(stats.dependency_count, 0);
    }
}
