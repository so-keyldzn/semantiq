//! Integration tests for the v5 -> v6 schema migration (MiniLM 384-dim ->
//! CodeRankEmbed 768-dim) and for the embedding-model reconciliation that
//! phase 2 runs before writing vectors (`IndexStore::prepare_embedding_space`).
//!
//! A vec0 column's dimension is fixed at CREATE time, so the migration must
//! DROP + recreate `chunks_vec`; distance observations and calibrated
//! thresholds are model-specific and must be cleared; a full re-index must be
//! forced. These tests seed an on-disk v5 database by hand, open it through
//! `IndexStore::open` (the production path) and inspect the result with a raw
//! connection.
//!
//! Outside that migration a model change only concerns the vectors (#86):
//! the structural index (files, symbols, references...) stays, every chunk
//! becomes pending again, and the stub never resets a real model's vectors.

use rusqlite::{Connection, params};
use semantiq_embeddings::STUB_EMBEDDING_MODEL_ID;
use semantiq_index::schema::{EMBEDDING_DIMENSION, SCHEMA_VERSION, embedding_model_id};
use semantiq_index::{EmbeddingSpace, IndexStore};
use semantiq_parser::{Symbol, SymbolKind};
use std::path::Path;
use tempfile::TempDir;

/// Bootstrap the sqlite-vec extension once, process-wide.
fn register_sqlite_vec() {
    let _ = IndexStore::open_in_memory().expect("bootstrap IndexStore registers sqlite-vec");
}

/// The DDL of a v5 database, with `chunks_vec` at the old 384 dimension.
const V5_SCHEMA: &str = r#"
    CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
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
        resolved_path TEXT,
        FOREIGN KEY (source_file_id) REFERENCES files(id) ON DELETE CASCADE
    );
    CREATE TABLE distance_observations (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        language TEXT NOT NULL,
        distance REAL NOT NULL,
        query_hash INTEGER NOT NULL,
        timestamp INTEGER NOT NULL,
        UNIQUE(query_hash, language)
    );
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
    CREATE VIRTUAL TABLE chunks_vec USING vec0(
        chunk_id INTEGER PRIMARY KEY,
        embedding float[384]
    );
"#;

fn embedding_blob(dim: usize, seed: f32) -> Vec<u8> {
    (0..dim)
        .flat_map(|i| (seed + i as f32 * 0.001).to_le_bytes())
        .collect()
}

/// Seed a v5 database with one indexed file (chunk + 384-d vector), distance
/// observations, a calibrated threshold and a current parser version.
fn seed_v5_db(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    conn.execute_batch(V5_SCHEMA).unwrap();
    conn.execute_batch(
        "INSERT INTO metadata (key, value) VALUES ('schema_version', '5');
         INSERT INTO metadata (key, value) VALUES ('parser_version', '9');
         INSERT INTO files (path, language, hash, size, last_modified, indexed_at)
         VALUES ('src/lib.rs', 'rust', 'h1', 10, 1000, 2000);
         INSERT INTO chunks (file_id, content, start_line, end_line, start_byte, end_byte, symbols_json)
         VALUES (1, 'fn lib() {}', 1, 1, 0, 11, '[]');
         INSERT INTO distance_observations (language, distance, query_hash, timestamp)
         VALUES ('rust', 0.8, 1, 1000), ('rust', 0.9, 2, 1000);
         INSERT INTO threshold_calibration
            (language, max_distance, min_similarity, confidence, sample_count, calibrated_at)
         VALUES ('rust', 1.1, 0.35, 'high', 600, 1000);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chunks_vec(chunk_id, embedding) VALUES (1, ?1)",
        params![embedding_blob(384, 0.1)],
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

fn chunks_vec_sql(conn: &Connection) -> String {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE name = 'chunks_vec'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn migrate_v5_to_v6_recreates_chunks_vec_and_forces_reindex() {
    register_sqlite_vec();
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");
    seed_v5_db(&db);

    let store = IndexStore::open(&db).unwrap();
    // parser_version was removed: the next startup performs a full re-index.
    assert!(store.needs_full_reindex().unwrap());
    assert!(store.check_and_prepare_for_reindex().unwrap());

    // A 768-d vector is accepted by the recreated table.
    let file_id = store
        .insert_file("src/new.rs", Some("rust"), "fn new() {}", 1000)
        .unwrap();
    store
        .insert_chunks(
            file_id,
            &[semantiq_parser::CodeChunk {
                content: "fn new() {}".to_string(),
                start_line: 1,
                end_line: 1,
                start_byte: 0,
                end_byte: 11,
                symbols: vec!["new".to_string()],
            }],
        )
        .unwrap();
    let chunk_id = store.get_chunks_by_file(file_id).unwrap()[0].id;
    store
        .store_chunk_embeddings(&[(chunk_id, vec![0.1; EMBEDDING_DIMENSION].as_slice())])
        .unwrap();
    drop(store);

    let conn = Connection::open(&db).unwrap();
    let sql = chunks_vec_sql(&conn);
    assert!(
        sql.contains(&format!("float[{EMBEDDING_DIMENSION}]")),
        "chunks_vec not recreated at the new dimension: {sql}"
    );
    assert_eq!(EMBEDDING_DIMENSION, 768);
    assert_eq!(count(&conn, "distance_observations"), 0);
    assert_eq!(count(&conn, "threshold_calibration"), 0);
    // Old file gone; only the one inserted after the migration remains.
    assert_eq!(count(&conn, "files"), 1);
    assert_eq!(count(&conn, "chunks_vec"), 1);
    assert_eq!(
        metadata(&conn, "schema_version"),
        Some(SCHEMA_VERSION.to_string())
    );
    assert_eq!(
        metadata(&conn, "embedding_model").as_deref(),
        Some(embedding_model_id())
    );
    assert_eq!(
        metadata(&conn, "embedding_dim"),
        Some(EMBEDDING_DIMENSION.to_string())
    );
}

#[test]
fn reopening_with_same_model_keeps_data() {
    register_sqlite_vec();
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");

    let store = IndexStore::open(&db).unwrap();
    store.set_parser_version().unwrap();
    store
        .insert_file("src/a.rs", Some("rust"), "fn a() {}", 1000)
        .unwrap();
    drop(store);

    let store = IndexStore::open(&db).unwrap();
    assert!(!store.needs_full_reindex().unwrap());
    assert!(store.get_file_by_path("src/a.rs").unwrap().is_some());
    assert_eq!(
        store.prepare_embedding_space().unwrap(),
        EmbeddingSpace::Current
    );
}

/// A file with one symbol, one embedded chunk and one pending chunk, plus a
/// distance observation; `parser_version` stamped.
fn seed_embedded_index(db: &Path) {
    let store = IndexStore::open(db).unwrap();
    store.set_parser_version().unwrap();
    let file_id = store
        .insert_file("src/a.rs", Some("rust"), "fn a() {}\nfn b() {}", 1000)
        .unwrap();
    store
        .insert_symbols(
            file_id,
            &[Symbol {
                name: "a".into(),
                kind: SymbolKind::Function,
                start_line: 1,
                end_line: 1,
                start_byte: 0,
                end_byte: 9,
                signature: None,
                doc_comment: None,
                parent: None,
            }],
        )
        .unwrap();
    let chunk = |content: &str, line: usize| semantiq_parser::CodeChunk {
        content: content.to_string(),
        start_line: line,
        end_line: line,
        start_byte: 0,
        end_byte: 9,
        symbols: vec![],
    };
    store
        .insert_chunks(file_id, &[chunk("fn a() {}", 1), chunk("fn b() {}", 2)])
        .unwrap();
    let chunk_id = store.get_chunks_by_file(file_id).unwrap()[0].id;
    store
        .store_chunk_embeddings(&[(chunk_id, vec![0.1; EMBEDDING_DIMENSION].as_slice())])
        .unwrap();
    drop(store);
    Connection::open(db)
        .unwrap()
        .execute_batch(
            "INSERT INTO distance_observations (language, distance, query_hash, timestamp)
             VALUES ('rust', 0.8, 1, 1000);",
        )
        .unwrap();
}

/// Regression for #86: opening an index stamped with another model (any
/// command, `semantiq stats` included) changes nothing at all.
#[test]
fn opening_with_another_model_touches_nothing() {
    register_sqlite_vec();
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");
    seed_embedded_index(&db);
    Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE metadata SET value = 'some/other-model' WHERE key = 'embedding_model'",
            [],
        )
        .unwrap();

    let store = IndexStore::open(&db).unwrap();
    assert!(!store.needs_full_reindex().unwrap());
    assert!(store.get_file_by_path("src/a.rs").unwrap().is_some());
    assert_eq!(store.find_symbol_by_name("a").unwrap().len(), 1);
    drop(store);

    let conn = Connection::open(&db).unwrap();
    assert_eq!(count(&conn, "chunks_vec"), 1);
    assert_eq!(count(&conn, "distance_observations"), 1);
    assert_eq!(
        metadata(&conn, "embedding_model").as_deref(),
        Some("some/other-model")
    );
}

/// A real model finding another model's vectors drops them and makes every
/// chunk pending again; files, symbols and the parser version stay.
#[test]
fn real_model_change_resets_vectors_and_keeps_the_structure() {
    register_sqlite_vec();
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");
    seed_embedded_index(&db);

    let store = IndexStore::open(&db).unwrap();
    assert_eq!(
        store
            .prepare_embedding_space_for("vendor/new-model", false)
            .unwrap(),
        EmbeddingSpace::Reset {
            previous: embedding_model_id().to_string()
        }
    );
    assert!(!store.needs_full_reindex().unwrap());
    assert!(store.get_file_by_path("src/a.rs").unwrap().is_some());
    assert_eq!(store.find_symbol_by_name("a").unwrap().len(), 1);
    let counts = store.embedding_counts().unwrap();
    assert_eq!((counts.total, counts.pending()), (2, 2));
    // Settled: the next pass finds its own model.
    assert_eq!(
        store
            .prepare_embedding_space_for("vendor/new-model", false)
            .unwrap(),
        EmbeddingSpace::Current
    );
    drop(store);

    let conn = Connection::open(&db).unwrap();
    assert_eq!(count(&conn, "chunks_vec"), 0);
    assert_eq!(count(&conn, "chunks"), 2);
    assert_eq!(count(&conn, "distance_observations"), 0);
    assert_eq!(count(&conn, "threshold_calibration"), 0);
    assert_eq!(
        metadata(&conn, "embedding_model").as_deref(),
        Some("vendor/new-model")
    );
    assert_eq!(
        metadata(&conn, "embedding_dim"),
        Some(EMBEDDING_DIMENSION.to_string())
    );
    assert_eq!(
        metadata(&conn, "parser_version"),
        Some(semantiq_parser::PARSER_VERSION.to_string())
    );
    let sql = chunks_vec_sql(&conn);
    assert!(
        sql.contains(&format!("float[{EMBEDDING_DIMENSION}]")),
        "{sql}"
    );
}

/// The stub (a build without `onnx`, `SEMANTIQ_EMBEDDINGS=stub`) must never
/// destroy a real model's vectors: it reports them foreign and writes nothing.
#[test]
fn stub_never_resets_a_real_models_vectors() {
    register_sqlite_vec();
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");
    seed_embedded_index(&db);
    Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE metadata SET value = 'vendor/real-model' WHERE key = 'embedding_model'",
            [],
        )
        .unwrap();

    let store = IndexStore::open(&db).unwrap();
    assert_eq!(
        store
            .prepare_embedding_space_for(STUB_EMBEDDING_MODEL_ID, true)
            .unwrap(),
        EmbeddingSpace::Foreign {
            model: "vendor/real-model".to_string()
        }
    );
    drop(store);

    let conn = Connection::open(&db).unwrap();
    assert_eq!(count(&conn, "chunks_vec"), 1);
    assert_eq!(
        metadata(&conn, "embedding_model").as_deref(),
        Some("vendor/real-model")
    );
}

/// A stub-built index (zero vectors) opened by a real model is reset: the
/// zero vectors must not survive as "already embedded".
#[test]
fn real_model_resets_a_stub_built_index() {
    register_sqlite_vec();
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");
    seed_embedded_index(&db);
    Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'embedding_model'",
            params![STUB_EMBEDDING_MODEL_ID],
        )
        .unwrap();

    let store = IndexStore::open(&db).unwrap();
    assert_eq!(
        store
            .prepare_embedding_space_for("vendor/real-model", false)
            .unwrap(),
        EmbeddingSpace::Reset {
            previous: STUB_EMBEDDING_MODEL_ID.to_string()
        }
    );
    assert_eq!(store.embedding_counts().unwrap().pending(), 2);
    assert!(store.get_file_by_path("src/a.rs").unwrap().is_some());
}

/// A dimension mismatch means the table cannot even take this build's
/// vectors: reset whoever asks, stub included.
#[test]
fn dimension_mismatch_resets_even_for_the_stub() {
    register_sqlite_vec();
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");
    seed_embedded_index(&db);
    Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE metadata SET value = '384' WHERE key = 'embedding_dim'",
            [],
        )
        .unwrap();

    let store = IndexStore::open(&db).unwrap();
    assert!(matches!(
        store
            .prepare_embedding_space_for(STUB_EMBEDDING_MODEL_ID, true)
            .unwrap(),
        EmbeddingSpace::Reset { .. }
    ));
    drop(store);
    let conn = Connection::open(&db).unwrap();
    assert_eq!(
        metadata(&conn, "embedding_dim"),
        Some(EMBEDDING_DIMENSION.to_string())
    );
    assert_eq!(count(&conn, "chunks_vec"), 0);
}
