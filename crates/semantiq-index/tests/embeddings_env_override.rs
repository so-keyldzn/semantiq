//! `SEMANTIQ_EMBEDDINGS=stub` must reset an index written by the real model.
//!
//! The variable switches an ONNX build to the zero-vector stub at runtime, so
//! the effective model id is resolved at runtime too: opening a database marked
//! `CodeRankEmbed` with the override set must rebuild the vector index instead
//! of mixing zero vectors with real ones. Kept in its own test binary because
//! it mutates the process environment (single test, no concurrent readers).

use rusqlite::{Connection, params};
use semantiq_embeddings::{
    CODERANKEMBED_MODEL_ID, EMBEDDINGS_ENV_VAR, STUB_EMBEDDING_MODEL_ID, embedding_model_id,
};
use semantiq_index::IndexStore;
use semantiq_index::schema::EMBEDDING_DIMENSION;
use tempfile::TempDir;

fn metadata(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM metadata WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .ok()
}

#[test]
fn stub_env_override_resets_coderankembed_index() {
    // SAFETY: only test in this binary; no other thread reads the environment.
    unsafe { std::env::set_var(EMBEDDINGS_ENV_VAR, "stub") };
    assert_eq!(embedding_model_id(), STUB_EMBEDDING_MODEL_ID);

    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");

    let store = IndexStore::open(&db).unwrap();
    store.set_parser_version().unwrap();
    let file_id = store
        .insert_file("src/a.rs", Some("rust"), "fn a() {}", 1000)
        .unwrap();
    store
        .insert_chunks(
            file_id,
            &[semantiq_parser::CodeChunk {
                content: "fn a() {}".to_string(),
                start_line: 1,
                end_line: 1,
                start_byte: 0,
                end_byte: 9,
                symbols: vec!["a".to_string()],
            }],
        )
        .unwrap();
    let chunk_id = store.get_chunks_by_file(file_id).unwrap()[0].id;
    store
        .update_chunk_embedding(chunk_id, &vec![0.1; EMBEDDING_DIMENSION])
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

    let store = IndexStore::open(&db).unwrap();
    assert!(store.needs_full_reindex().unwrap());
    assert!(store.get_file_by_path("src/a.rs").unwrap().is_none());
    drop(store);

    let conn = Connection::open(&db).unwrap();
    let vectors: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks_vec", [], |row| row.get(0))
        .unwrap();
    assert_eq!(vectors, 0);
    assert_eq!(
        metadata(&conn, "embedding_model").as_deref(),
        Some(STUB_EMBEDDING_MODEL_ID)
    );
}
