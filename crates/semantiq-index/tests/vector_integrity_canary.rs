//! `IndexStore::warn_on_vector_integrity_loss` is the chunks / chunks_vec
//! canary: it runs once per `serve` (#164) and must speak up on either kind
//! of loss. Events are captured with a minimal `tracing` subscriber.

use rusqlite::{Connection, params};
use semantiq_index::IndexStore;
use semantiq_index::schema::EMBEDDING_DIMENSION;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

#[derive(Clone, Default)]
struct Warnings(Arc<Mutex<Vec<String>>>);

impl Subscriber for Warnings {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= Level::WARN
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        struct Collect(String);
        impl Visit for Collect {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                let _ = write!(self.0, "{}={:?} ", field.name(), value);
            }
        }
        let mut collect = Collect(String::new());
        event.record(&mut collect);
        self.0.lock().unwrap().push(collect.0);
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

fn embedding_blob() -> Vec<u8> {
    vec![0.1f32; EMBEDDING_DIMENSION]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

#[test]
fn canary_warns_on_orphan_vectors_and_on_missing_vectors() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");
    let store = IndexStore::open(&db).unwrap();
    let file_id = store
        .insert_file("a.rs", Some("rust"), "fn a() {}", 1)
        .unwrap();
    store
        .insert_chunks(
            file_id,
            &[semantiq_parser::CodeChunk {
                content: "fn a() {}".into(),
                start_line: 1,
                end_line: 1,
                start_byte: 0,
                end_byte: 9,
                symbols: vec![],
            }],
        )
        .unwrap();

    // Healthy: silent.
    let warnings = Warnings::default();
    tracing::subscriber::with_default(warnings.clone(), || {
        store.warn_on_vector_integrity_loss();
    });
    assert!(warnings.0.lock().unwrap().is_empty());

    // Corrupt the pairing from outside (sqlite-vec is registered process-wide
    // by the store above): one orphan vector, one embedded chunk without one.
    let conn = Connection::open(&db).unwrap();
    conn.execute(
        "INSERT INTO chunks_vec(chunk_id, embedding) VALUES (999, ?1)",
        params![embedding_blob()],
    )
    .unwrap();
    conn.execute(
        "UPDATE chunks SET embedding = ?1 WHERE file_id = ?2",
        params![embedding_blob(), file_id],
    )
    .unwrap();
    drop(conn);

    let warnings = Warnings::default();
    tracing::subscriber::with_default(warnings.clone(), || {
        store.warn_on_vector_integrity_loss();
    });
    let warnings = warnings.0.lock().unwrap();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[0].contains("orphan_chunk_vectors=1"),
        "{warnings:?}"
    );
    assert!(
        warnings[1].contains("chunks_missing_vectors=1"),
        "{warnings:?}"
    );
}
