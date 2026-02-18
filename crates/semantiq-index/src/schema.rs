use rusqlite::{Connection, Result as SqliteResult};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: i32 = 4;

/// Embedding dimension (MiniLM-L6-v2 produces 384-dimensional vectors)
pub const EMBEDDING_DIMENSION: usize = 384;

/// Read the current schema version from the database.
/// Returns 0 if the metadata table doesn't exist yet (fresh database).
fn get_stored_schema_version(conn: &Connection) -> SqliteResult<i32> {
    // Check if metadata table exists
    let table_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='metadata'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;

    if !table_exists {
        return Ok(0);
    }

    // Read schema_version from metadata
    match conn.query_row(
        "SELECT value FROM metadata WHERE key = 'schema_version'",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(val) => Ok(val.parse::<i32>().unwrap_or(0)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(e) => Err(e),
    }
}

/// Apply incremental migrations to bring an existing database up to the current schema.
/// This must be called BEFORE `init_schema()` on existing databases.
pub fn migrate_schema(conn: &Connection) -> SqliteResult<()> {
    let stored = get_stored_schema_version(conn)?;

    if stored == 0 || stored >= SCHEMA_VERSION {
        // Fresh database or already up to date — nothing to migrate
        return Ok(());
    }

    // v3 -> v4: add resolved_path column to dependencies
    if stored < 4 {
        tracing::info!(
            "Migrating schema v{} -> v4: adding resolved_path column",
            stored
        );
        conn.execute_batch(
            "ALTER TABLE dependencies ADD COLUMN resolved_path TEXT;
             CREATE INDEX IF NOT EXISTS idx_deps_resolved ON dependencies(resolved_path);",
        )?;
    }

    // Future migrations go here:
    // if stored < 5 { ... }

    Ok(())
}

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
            resolved_path TEXT,
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
        CREATE INDEX IF NOT EXISTS idx_deps_resolved ON dependencies(resolved_path);

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

        -- Distance observations for threshold calibration
        -- Records distances observed during semantic search for ML-based calibration
        CREATE TABLE IF NOT EXISTS distance_observations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            language TEXT NOT NULL,
            distance REAL NOT NULL,
            query_hash INTEGER NOT NULL,
            timestamp INTEGER NOT NULL,
            UNIQUE(query_hash, language)
        );

        CREATE INDEX IF NOT EXISTS idx_obs_language ON distance_observations(language);
        CREATE INDEX IF NOT EXISTS idx_obs_timestamp ON distance_observations(timestamp);

        -- Calibrated thresholds per language
        -- Stores ML-calibrated thresholds based on observed distance distributions
        CREATE TABLE IF NOT EXISTS threshold_calibration (
            language TEXT PRIMARY KEY,
            max_distance REAL NOT NULL,
            min_similarity REAL NOT NULL,
            confidence TEXT NOT NULL,
            sample_count INTEGER NOT NULL,
            p50_distance REAL,
            p90_distance REAL,
            p95_distance REAL,
            mean_distance REAL,
            std_distance REAL,
            calibrated_at INTEGER NOT NULL
        );
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
    pub resolved_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn test_migrate_v3_to_v4() {
        // Simulate a v3 database: create dependencies table WITHOUT resolved_path
        crate::store::init_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();

        // Create v3 schema (dependencies without resolved_path)
        conn.execute_batch(
            r#"
            CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO metadata (key, value) VALUES ('schema_version', '3');

            CREATE TABLE files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                language TEXT,
                hash TEXT NOT NULL,
                size INTEGER NOT NULL,
                last_modified INTEGER NOT NULL,
                indexed_at INTEGER NOT NULL
            );

            CREATE TABLE symbols (
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

            CREATE TABLE chunks (
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

            CREATE TABLE dependencies (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_file_id INTEGER NOT NULL,
                target_path TEXT NOT NULL,
                import_name TEXT,
                kind TEXT NOT NULL,
                FOREIGN KEY (source_file_id) REFERENCES files(id) ON DELETE CASCADE
            );
            "#,
        )
        .unwrap();

        // Insert a v3 dependency (no resolved_path column)
        conn.execute(
            "INSERT INTO files (path, language, hash, size, last_modified, indexed_at)
             VALUES ('test.rs', 'rust', 'abc', 10, 1000, 2000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO dependencies (source_file_id, target_path, import_name, kind)
             VALUES (1, 'crate::utils', 'utils', 'local')",
            [],
        )
        .unwrap();

        // Run migration — should add resolved_path column
        migrate_schema(&conn).unwrap();

        // Run init_schema — should succeed now (CREATE INDEX on resolved_path won't fail)
        init_schema(&conn).unwrap();

        // Verify the column exists and old data is preserved with NULL resolved_path
        let (target, resolved): (String, Option<String>) = conn
            .query_row(
                "SELECT target_path, resolved_path FROM dependencies WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, "crate::utils");
        assert!(resolved.is_none());

        // Verify we can insert with resolved_path
        conn.execute(
            "INSERT INTO dependencies (source_file_id, target_path, import_name, kind, resolved_path)
             VALUES (1, 'crate::schema', 'schema', 'local', 'src/schema.rs')",
            [],
        )
        .unwrap();

        let resolved: Option<String> = conn
            .query_row(
                "SELECT resolved_path FROM dependencies WHERE id = 2",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(resolved.as_deref(), Some("src/schema.rs"));

        // Schema version should now be 4
        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn test_migrate_fresh_db_is_noop() {
        crate::store::init_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();

        // On a fresh DB (no metadata table), migrate should be a no-op
        migrate_schema(&conn).unwrap();
        // init_schema should work normally
        init_schema(&conn).unwrap();

        let version: String = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn test_migrate_already_v4_is_noop() {
        // A v4 database should not be touched by migration
        let store = IndexStore::open_in_memory().unwrap();

        // Insert a dependency with resolved_path
        let file_id = store
            .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
            .unwrap();
        store
            .insert_dependency(
                file_id,
                "crate::utils",
                Some("utils"),
                "local",
                Some("src/utils.rs"),
            )
            .unwrap();

        // Running migrate on a v4 store should not error
        store
            .with_conn(|conn| {
                migrate_schema(conn)?;
                Ok(())
            })
            .unwrap();

        // Data should be intact
        let deps = store.get_dependencies(file_id).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].resolved_path.as_deref(), Some("src/utils.rs"));
    }
}
