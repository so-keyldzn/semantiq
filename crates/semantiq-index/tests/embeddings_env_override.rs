//! `SEMANTIQ_EMBEDDINGS=stub` must leave an index written by the real model alone.
//!
//! The variable switches an ONNX build to the zero-vector stub at runtime, so
//! the effective model id is resolved at runtime too. Opening a database
//! marked `CodeRankEmbed` with the override set used to wipe the whole index
//! (#86): `SEMANTIQ_EMBEDDINGS=stub semantiq stats`, documented as "semantic
//! search off, no download", dropped 20 minutes of phase 2 and every symbol.
//! Now the structural index and the vectors stay, and the stub's phase 2
//! writes nothing rather than mixing zero vectors with real ones. Kept in its
//! own test binary because it mutates the process environment (single test,
//! no concurrent readers).

use rusqlite::{Connection, params};
use semantiq_embeddings::{
    CODERANKEMBED_MODEL_ID, EMBEDDINGS_ENV_VAR, STUB_EMBEDDING_MODEL_ID, StubEmbeddingModel,
    embedding_model_id,
};
use semantiq_index::schema::EMBEDDING_DIMENSION;
use semantiq_index::{EmbeddingProgress, EmbeddingSpace, IndexStore, embed_pending};
use tempfile::TempDir;

fn metadata(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM metadata WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .ok()
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

#[test]
fn stub_env_override_leaves_coderankembed_index_untouched() {
    // SAFETY: only test in this binary; no other thread reads the environment.
    unsafe { std::env::set_var(EMBEDDINGS_ENV_VAR, "stub") };
    assert_eq!(embedding_model_id(), STUB_EMBEDDING_MODEL_ID);

    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");

    let store = IndexStore::open(&db).unwrap();
    store.set_parser_version().unwrap();
    let file_id = store
        .insert_file("src/a.rs", Some("rust"), "fn a() {}\nfn b() {}", 1000)
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

    // Pretend the index was written by the real model.
    Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'embedding_model'",
            params![CODERANKEMBED_MODEL_ID],
        )
        .unwrap();

    // Opening (any command) changes nothing.
    let store = IndexStore::open(&db).unwrap();
    assert!(!store.needs_full_reindex().unwrap());
    assert!(store.get_file_by_path("src/a.rs").unwrap().is_some());

    // Phase 2 under the stub sees a foreign space and writes nothing: the
    // pending chunk stays pending, the real vector stays.
    assert_eq!(
        store.prepare_embedding_space().unwrap(),
        EmbeddingSpace::Foreign {
            model: CODERANKEMBED_MODEL_ID.to_string()
        }
    );
    let progress = EmbeddingProgress::new();
    let result = embed_pending(
        &store,
        &StubEmbeddingModel::new(),
        32,
        &progress,
        &|| false,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(result.embedded, 0);
    assert_eq!(store.embedding_counts().unwrap().pending(), 1);
    drop(store);

    let conn = Connection::open(&db).unwrap();
    assert_eq!(count(&conn, "chunks_vec"), 1);
    assert_eq!(count(&conn, "files"), 1);
    assert_eq!(
        metadata(&conn, "embedding_model").as_deref(),
        Some(CODERANKEMBED_MODEL_ID)
    );
}
