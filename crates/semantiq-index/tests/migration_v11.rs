//! Integration tests for the v10 -> v11 schema migration (#162).
//!
//! v11 compacts `refs`: names are interned in `names` and `kind` becomes a
//! 1-byte code, and `idx_files_path` (a duplicate of the UNIQUE index on
//! `files.path`) is dropped. The old `refs` is dropped and recreated empty by
//! `init_schema()`; removing `parser_version` makes the next start run a full
//! phase 1 that refills it, while files, chunks and their embeddings stay.
//!
//! The v10 database is built by hand in SQL (the public store API only opens
//! the current version) and inspected with a raw connection.

use rusqlite::{Connection, params};
use semantiq_index::IndexStore;
use semantiq_index::schema::{
    EMBEDDING_DIMENSION, SCHEMA_VERSION, embedding_model_id, migrate_schema_to,
};
use semantiq_parser::{Reference, ReferenceKind};
use std::path::Path;
use tempfile::TempDir;

/// Bootstrap the sqlite-vec extension once, process-wide.
fn register_sqlite_vec() {
    let _ = IndexStore::open_in_memory().expect("bootstrap IndexStore registers sqlite-vec");
}

/// The DDL of a v10 database, as `init_schema()` wrote it at v0.12.0.
const V10_SCHEMA: &str = r#"
    CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE files (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        path TEXT NOT NULL UNIQUE,
        language TEXT,
        hash TEXT NOT NULL,
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
        resolved_path TEXT,
        line INTEGER NOT NULL DEFAULT 0,
        end_line INTEGER NOT NULL DEFAULT 0,
        FOREIGN KEY (source_file_id) REFERENCES files(id) ON DELETE CASCADE
    );
    CREATE TABLE refs (
        name TEXT NOT NULL,
        file_id INTEGER NOT NULL,
        line INTEGER NOT NULL,
        kind TEXT NOT NULL,
        count INTEGER NOT NULL DEFAULT 1,
        PRIMARY KEY (name, file_id, line),
        FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
    ) WITHOUT ROWID;
    CREATE TABLE call_edges (
        file_id INTEGER NOT NULL,
        line INTEGER NOT NULL,
        callee TEXT NOT NULL,
        caller TEXT NOT NULL,
        caller_line INTEGER NOT NULL,
        count INTEGER NOT NULL DEFAULT 1,
        PRIMARY KEY (file_id, line, callee),
        FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
    ) WITHOUT ROWID;
    CREATE TABLE type_relations (
        file_id INTEGER NOT NULL,
        line INTEGER NOT NULL,
        type_name TEXT NOT NULL,
        super_name TEXT NOT NULL,
        kind TEXT NOT NULL,
        end_line INTEGER NOT NULL,
        PRIMARY KEY (file_id, line, type_name, super_name),
        FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
    ) WITHOUT ROWID;
    CREATE INDEX idx_files_path ON files(path);
    CREATE INDEX idx_symbols_name ON symbols(name);
    CREATE INDEX idx_symbols_kind ON symbols(kind);
    CREATE INDEX idx_symbols_file_id ON symbols(file_id);
    CREATE INDEX idx_chunks_file_id ON chunks(file_id);
    CREATE INDEX idx_chunks_pending ON chunks(id) WHERE embedding IS NULL;
    CREATE INDEX idx_deps_source ON dependencies(source_file_id);
    CREATE INDEX idx_deps_target ON dependencies(target_path);
    CREATE INDEX idx_deps_resolved ON dependencies(resolved_path);
    CREATE INDEX idx_refs_file_id ON refs(file_id);
    CREATE INDEX idx_call_edges_callee ON call_edges(callee);
    CREATE INDEX idx_call_edges_caller ON call_edges(caller);
    CREATE INDEX idx_type_relations_type ON type_relations(type_name);
    CREATE INDEX idx_type_relations_super ON type_relations(super_name);
    CREATE VIRTUAL TABLE symbols_fts USING fts5(
        name, signature, doc_comment, content='symbols', content_rowid='id'
    );
    CREATE TRIGGER symbols_ai AFTER INSERT ON symbols BEGIN
        INSERT INTO symbols_fts(rowid, name, signature, doc_comment)
        VALUES (new.id, new.name, new.signature, new.doc_comment);
    END;
    CREATE TRIGGER symbols_ad AFTER DELETE ON symbols BEGIN
        INSERT INTO symbols_fts(symbols_fts, rowid, name, signature, doc_comment)
        VALUES ('delete', old.id, old.name, old.signature, old.doc_comment);
    END;
    CREATE TRIGGER symbols_au AFTER UPDATE ON symbols BEGIN
        INSERT INTO symbols_fts(symbols_fts, rowid, name, signature, doc_comment)
        VALUES ('delete', old.id, old.name, old.signature, old.doc_comment);
        INSERT INTO symbols_fts(rowid, name, signature, doc_comment)
        VALUES (new.id, new.name, new.signature, new.doc_comment);
    END;
    CREATE TABLE distance_observations (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        language TEXT NOT NULL,
        distance REAL NOT NULL,
        query_hash INTEGER NOT NULL,
        timestamp INTEGER NOT NULL,
        UNIQUE(query_hash, language)
    );
    CREATE INDEX idx_obs_language ON distance_observations(language);
    CREATE INDEX idx_obs_timestamp ON distance_observations(timestamp);
    CREATE TABLE threshold_calibration (
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
"#;

fn embedding_blob(seed: f32) -> Vec<u8> {
    (0..EMBEDDING_DIMENSION)
        .flat_map(|i| (seed + i as f32 * 0.001).to_le_bytes())
        .collect()
}

/// A v10 database with one indexed file: a symbol, an embedded chunk (with
/// its vector), two references in the old text shape and a call edge.
fn seed_v10_db(conn: &Connection) {
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    conn.execute_batch(V10_SCHEMA).unwrap();
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE chunks_vec USING vec0(
            chunk_id INTEGER PRIMARY KEY,
            embedding float[{EMBEDDING_DIMENSION}]
        );"
    ))
    .unwrap();
    conn.execute(
        "INSERT INTO metadata (key, value) VALUES
            ('schema_version', '10'), ('parser_version', '17'),
            ('embedding_model', ?1), ('embedding_dim', ?2)",
        params![embedding_model_id(), EMBEDDING_DIMENSION.to_string()],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO files (path, language, hash, last_modified, indexed_at)
         VALUES ('src/lib.rs', 'rust', 'h1', 1000, 2000);
         INSERT INTO symbols (file_id, name, kind, start_line, end_line)
         VALUES (1, 'helper', 'function', 1, 3);
         INSERT INTO chunks (file_id, content, start_line, end_line, symbols_json)
         VALUES (1, 'fn helper() {}', 1, 3, '[\"helper\"]');
         INSERT INTO refs (name, file_id, line, kind, count) VALUES
            ('helper', 1, 1, 'definition', 1), ('helper', 1, 5, 'call', 2);
         INSERT INTO call_edges (file_id, line, callee, caller, caller_line, count)
         VALUES (1, 5, 'helper', 'main', 4, 2);",
    )
    .unwrap();
    conn.execute(
        "UPDATE chunks SET embedding = ?1 WHERE id = 1",
        params![embedding_blob(0.1)],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chunks_vec(chunk_id, embedding) VALUES (1, ?1)",
        params![embedding_blob(0.1)],
    )
    .unwrap();
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn metadata(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM metadata WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .ok()
}

fn schema_object(conn: &Connection, name: &str) -> Option<String> {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE name = ?1",
        params![name],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

#[test]
fn migrate_v10_to_v11_drops_duplicate_index_and_old_refs_keeping_the_rest() {
    register_sqlite_vec();
    let conn = Connection::open_in_memory().unwrap();
    seed_v10_db(&conn);
    assert!(schema_object(&conn, "idx_files_path").is_some());

    migrate_schema_to(&conn, 11).unwrap();

    assert_eq!(metadata(&conn, "schema_version"), Some("11".to_string()));
    assert!(schema_object(&conn, "idx_files_path").is_none());
    // The UNIQUE constraint's own index still serves lookups by path.
    assert!(schema_object(&conn, "sqlite_autoindex_files_1").is_none()); // auto indexes have no sql
    let autoindex: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'sqlite_autoindex_files_1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(autoindex, 1);
    // The old refs table is gone; init_schema recreates it in the new shape.
    assert!(schema_object(&conn, "refs").is_none());
    assert!(schema_object(&conn, "idx_refs_file_id").is_none());
    // Everything else stays, embeddings included.
    assert_eq!(count(&conn, "files"), 1);
    assert_eq!(count(&conn, "symbols"), 1);
    assert_eq!(count(&conn, "chunks"), 1);
    assert_eq!(count(&conn, "chunks_vec"), 1);
    assert_eq!(count(&conn, "call_edges"), 1);
    // A full phase 1 is due: it refills refs.
    assert_eq!(metadata(&conn, "parser_version"), None);

    // Idempotent.
    migrate_schema_to(&conn, 11).unwrap();
    assert_eq!(metadata(&conn, "schema_version"), Some("11".to_string()));
}

#[test]
fn opening_a_v10_index_reshapes_refs_and_keeps_embeddings_through_the_reindex() {
    register_sqlite_vec();
    let dir = TempDir::new().unwrap();
    let db: &Path = &dir.path().join("index.db");
    seed_v10_db(&Connection::open(db).unwrap());

    let store = IndexStore::open(db).unwrap();
    assert!(store.needs_full_reindex().unwrap());
    // What `serve` / `semantiq index` do next: prepare the full phase 1.
    assert!(store.check_and_prepare_for_reindex().unwrap());
    assert!(!store.needs_full_reindex().unwrap());

    // The file is still there, due for re-extraction; its embedded chunk too.
    let file = store.get_file_by_path("src/lib.rs").unwrap().unwrap();
    assert_eq!(file.hash, "");
    assert_eq!(store.embedding_counts().unwrap().pending(), 0);
    assert!(store.find_symbol_by_name("helper").unwrap().is_empty());
    assert!(
        store
            .find_references_by_name("helper", 10)
            .unwrap()
            .is_empty()
    );

    // Phase 1 writes references in the new shape and reads them back as words.
    store
        .insert_references(
            file.id,
            &[
                Reference {
                    name: "helper".into(),
                    line: 1,
                    kind: ReferenceKind::Definition,
                    count: 1,
                },
                Reference {
                    name: "helper".into(),
                    line: 5,
                    kind: ReferenceKind::Call,
                    count: 2,
                },
            ],
        )
        .unwrap();
    let refs = store.find_references_by_name("helper", 10).unwrap();
    let kinds: Vec<(i64, &str)> = refs.iter().map(|r| (r.line, r.kind.as_str())).collect();
    assert_eq!(kinds, vec![(1, "definition"), (5, "call")]);
    assert_eq!(store.count_usages("helper").unwrap(), 2);
    drop(store);

    let conn = Connection::open(db).unwrap();
    assert_eq!(
        metadata(&conn, "schema_version"),
        Some(SCHEMA_VERSION.to_string())
    );
    let refs_sql = schema_object(&conn, "refs").unwrap();
    assert!(refs_sql.contains("name_id INTEGER"), "{refs_sql}");
    assert!(refs_sql.contains("kind INTEGER"), "{refs_sql}");
    assert!(schema_object(&conn, "names").is_some());
    assert!(schema_object(&conn, "idx_refs_file_id").is_some());
    assert!(schema_object(&conn, "idx_files_path").is_none());
    assert_eq!(count(&conn, "names"), 1);
    let kind: i64 = conn
        .query_row("SELECT kind FROM refs WHERE line = 5", [], |row| row.get(0))
        .unwrap();
    assert_eq!(kind, 2, "call is stored as code 2");
    assert_eq!(count(&conn, "chunks_vec"), 1);
    assert!(metadata(&conn, "graph_generation").is_some());
}
