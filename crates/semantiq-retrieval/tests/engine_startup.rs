//! Building a `RetrievalEngine` must not scan `chunks` and `chunks_vec`.
//!
//! Every CLI command (`refs`, `deps`, `explain`...) builds an engine; the
//! chunks / chunks_vec integrity canary it used to run joined both tables in
//! full on each invocation (#164). The canary now belongs to `serve`
//! (`IndexStore::warn_on_vector_integrity_loss`). Observed through the
//! warnings: an index with an orphan vector builds an engine in silence.

use rusqlite::{Connection, params};
use semantiq_index::IndexStore;
use semantiq_index::schema::EMBEDDING_DIMENSION;
use semantiq_retrieval::RetrievalEngine;
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

#[test]
fn building_an_engine_does_not_run_the_vector_integrity_canary() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");
    let store = Arc::new(IndexStore::open(&db).unwrap());

    // An orphan vector, planted from outside (sqlite-vec is registered
    // process-wide by the store above).
    let embedding: Vec<u8> = vec![0.1f32; EMBEDDING_DIMENSION]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    Connection::open(&db)
        .unwrap()
        .execute(
            "INSERT INTO chunks_vec(chunk_id, embedding) VALUES (999, ?1)",
            params![embedding],
        )
        .unwrap();
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 1);

    let warnings = Warnings::default();
    tracing::subscriber::with_default(warnings.clone(), || {
        let _engine =
            RetrievalEngine::without_embeddings(Arc::clone(&store), dir.path().to_str().unwrap());
    });
    let warnings = warnings.0.lock().unwrap();
    assert!(
        warnings.iter().all(|w| !w.contains("orphan")),
        "engine construction ran the integrity canary: {warnings:?}"
    );

    // The canary itself still reports it, when asked.
    let warnings = Warnings::default();
    tracing::subscriber::with_default(warnings.clone(), || {
        store.warn_on_vector_integrity_loss();
    });
    assert!(
        warnings
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|w| w.contains("orphan_chunk_vectors=1")),
        "{:?}",
        warnings.0.lock().unwrap()
    );
}
